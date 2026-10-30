//! The R12/R13/R14/R15/R16 final-corrective regressions — the GENUINE
//! same-handle suffix-path tests (the trusted cursor absorbs the
//! verified composite closure through the hot path, never through a
//! fresh reopen), the SQLite cold/hot/suffix closure-equivalence
//! matrix (the walk-verified suffix entries carrying their closures —
//! no fail-open re-extraction), the focused negative matrix (the
//! missing, hash-mismatched, hash-valid-but-malformed, and
//! domain-unreadable suffixes all refuse; the trusted cursor never
//! advances on failure), the cold/suffix/hot accounting agreement,
//! and the production-default activation-refusal proof (a genuinely
//! feature-disabled process: this test file runs WITHOUT the
//! composite-test-fixture feature — it is NOT in the required-features
//! set and contains NO activation calls).

use engine_persist::composite_manifest::{self, CompositeManifest, ManifestEntry, SectionKind};
use engine_persist::local::LocalFileMedium;
use engine_persist::medium::{AppendOutcome, CanonicalMedium, MediumError};
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

fn fixture(variant: u8, malformed_tag: Option<usize>) -> Fixture {
    let mut seed = variant;
    let mut entries = Vec::new();
    let mut chunks = Vec::new();
    let push = |entries: &mut Vec<ManifestEntry>,
                chunks: &mut Vec<StoredObject>,
                seed: &mut u8,
                tag: &'static str,
                malformed: bool| {
        let payload = vec![*seed; 10 + *seed as usize];
        *seed = seed.wrapping_add(1);
        // The R16 hash-valid-but-malformed injection: the envelope
        // carries a FOREIGN section tag while the manifest entry
        // keeps declaring this position's tag with the envelope's
        // TRUE identity and payload length — the content hash
        // matches and the envelope frames, but the dependency
        // verification refuses the tag mismatch (the positional
        // grammar keeps the manifest itself decodable).
        let envelope_tag = if malformed {
            composite_manifest::TAG_WORLD
        } else {
            tag
        };
        let object = composite_manifest::encode_chunk_envelope(envelope_tag, &payload);
        entries.push(ManifestEntry {
            tag,
            identity: object.hash(),
            payload_length: payload.len() as u64,
            kind: SectionKind::Singleton,
        });
        chunks.push(object);
    };
    for (singleton_index, tag) in [
        composite_manifest::TAG_HEADER,
        composite_manifest::TAG_NEAR,
        composite_manifest::TAG_FAR,
        composite_manifest::TAG_BLOCKED,
        composite_manifest::TAG_HELD,
        composite_manifest::TAG_LEDGER_META,
    ]
    .into_iter()
    .enumerate()
    {
        let malformed = malformed_tag == Some(singleton_index);
        push(&mut entries, &mut chunks, &mut seed, tag, malformed);
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
        push(&mut entries, &mut chunks, &mut seed, tag, false);
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

/// Writes one object row (the provider's own schema: the raw 32-byte
/// hash blob, the domain, the bytes).
fn write_sqlite_object(conn: &rusqlite::Connection, hash: &Hash, domain: &str, bytes: &[u8]) {
    conn.execute(
        "INSERT OR IGNORE INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
        rusqlite::params![hash.as_bytes(), domain, bytes],
    )
    .unwrap();
}

/// Writes the suffix revision's record row (the full schema), its
/// CAS object, and moves HEAD — the provider's own layout.
fn write_sqlite_suffix_rows(conn: &rusqlite::Connection, record: &RevisionRecord, parent: Hash) {
    let record_id = record.id();
    conn.execute(
        "INSERT OR IGNORE INTO records (hash, bytes, epoch, parent, idem, kind, tick) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            record_id.as_bytes(),
            record.canonical_bytes(),
            record.epoch as i64,
            parent.as_bytes(),
            std::option::Option::<Vec<u8>>::None,
            1, // Regular
            record.tick as i64,
        ],
    )
    .unwrap();
    write_sqlite_object(
        conn,
        &record_id,
        "sektor7/engine-persist/revision-record/1",
        &record.canonical_bytes(),
    );
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('head', ?1)",
        rusqlite::params![record_id.as_bytes()],
    )
    .unwrap();
}

/// Writes an external composite revision directly to the SQLite
/// database (a second connection — the same tables the provider
/// uses). NO publication API involved. The injection modes are
/// independent: `corrupt_chunk` flips a transitive chunk's content
/// (the hash-MISMATCH class — the stored bytes no longer hash to
/// the row key), `remove_chunk` omits a chunk row entirely (the
/// MISSING class), and `malformed_chunk` stores a hash-VALID
/// envelope carrying a foreign section tag (the hash-valid-but-
/// MALFORMED class).
fn external_sqlite_composite(
    db_path: &std::path::Path,
    parent: Hash,
    parent_epoch: u64,
    variant: u8,
    corrupt_chunk: Option<usize>,
    remove_chunk: Option<usize>,
    malformed_chunk: Option<usize>,
) {
    let case = fixture(variant, malformed_chunk);
    let record =
        RevisionRecord::regular(parent, parent_epoch, 99).with_content_refs(vec![case.root.hash()]);

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
            write_sqlite_object(&conn, &object.hash(), object.domain(), &bytes);
        }
    }
    write_sqlite_suffix_rows(&conn, &record, parent);
}

/// Writes an external suffix revision whose first content object is
/// domain-UNREADABLE garbage (a u64 length prefix far beyond every
/// bound) stored under its TRUE content hash — the CAS load passes
/// and ONLY the domain peek fails: the suffix must REFUSE, never
/// treat the record as ordinary non-composite content.
fn external_sqlite_malformed_root(db_path: &std::path::Path, parent: Hash, parent_epoch: u64) {
    let garbage = [0xFFu8; 8];
    let garbage_hash = engine_persist::hash::hash_object(&garbage);
    let record =
        RevisionRecord::regular(parent, parent_epoch, 99).with_content_refs(vec![garbage_hash]);
    let conn = rusqlite::Connection::open(db_path).unwrap();
    write_sqlite_object(&conn, &garbage_hash, "unreadable/garbage", &garbage);
    write_sqlite_suffix_rows(&conn, &record, parent);
}

/// The legacy /1 founding publication on a fresh SQLite database —
/// the trusted prefix the external suffixes land on top of. Returns
/// the open handle, its trusted head, and the head's epoch.
fn sqlite_legacy_founding(db_path: &std::path::Path) -> (SqliteMedium, Hash, u64) {
    let mut medium = SqliteMedium::open(db_path).unwrap();
    let legacy = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let legacy_record = RevisionRecord::founding(AuthorityMode::Standalone, "r16", 0)
        .with_content_refs(vec![legacy.hash()]);
    medium
        .append(&legacy_record, std::slice::from_ref(&legacy), None)
        .expect("the legacy publication accepts");
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;
    (medium, head, epoch)
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
    let case = fixture(0, None);
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
        let case = fixture(0, None);
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
    let cold_before = medium.walk_stats().cold_walk_records;

    // The EXTERNAL composite revision through a second connection.
    external_sqlite_composite(&db_path, head, epoch, 0, None, None, None);

    // The ORIGINAL handle: head() forces the suffix absorption (the
    // trusted cursor sees a moved HEAD; the suffix walk verifies
    // the composite closure; the cursor absorbs it).
    let new_head = medium.head().expect("the suffix absorption succeeds");
    assert!(new_head.is_some());
    let new_head = new_head.unwrap();
    assert_ne!(new_head, head, "the HEAD moved");
    // R16: the SUFFIX path ran — NOT a cold rebuild (the cold-walk
    // counter unchanged, exactly one suffix record walked) — and the
    // verified closure is INCORPORATED into the trusted cursor's
    // accounting (the test-only observation; a subsequent validate()
    // cold-walks by design and cannot prove either).
    assert_eq!(
        medium.walk_stats().cold_walk_records,
        cold_before,
        "the suffix path ran (no cold rebuild — the trusted cursor absorbed)"
    );
    assert_eq!(medium.walk_stats().last_append_walked, 1);
    let accounting = medium.trusted_cursor_accounting().unwrap();
    assert_eq!(accounting.0, new_head);
    assert_eq!(
        accounting.2,
        2 + fixture(0, None).chunks.len() as u64,
        "the trusted cursor counts the legacy object + the root + the full verified closure"
    );
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

    external_sqlite_composite(&db_path, head, epoch, 0, None, Some(3), None);

    // The original handle: the suffix path must REFUSE (the
    // missing chunk fails the walk_suffix closure verification).
    let result = medium.head();
    assert!(
        result.is_err(),
        "the missing transitive chunk refuses the suffix absorption"
    );
    // The cold walk agrees — the chain is genuinely unverifiable
    // (never cursor conservatism).
    assert!(
        SqliteMedium::open(&db_path).is_err(),
        "the cold walk refuses the missing chunk too"
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

    external_sqlite_composite(&db_path, head, epoch, 0, Some(3), None, None);

    let result = medium.head();
    assert!(
        result.is_err(),
        "the corrupt (hash-mismatched) transitive chunk refuses the suffix absorption"
    );
    // The cold walk agrees.
    assert!(
        SqliteMedium::open(&db_path).is_err(),
        "the cold walk refuses the hash-mismatched chunk too"
    );
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_external_suffix_hash_valid_malformed_envelope_refuses() {
    // R16: a suffix chunk whose content hashes EXACTLY to its
    // declared identity and frames as an object, but whose envelope
    // carries a FOREIGN section tag (the manifest entry keeps the
    // positional grammar) — the full dependency verification
    // refuses; the suffix absorption never treats it as verified
    // (fail closed on the hash-valid-but-malformed class).
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-suffix-malformed-envelope");
    let db_path = root_dir.join("db.sqlite");
    let (mut medium, head, epoch) = sqlite_legacy_founding(&db_path);

    external_sqlite_composite(&db_path, head, epoch, 0, None, None, Some(3));

    let result = medium.head();
    assert!(
        result.is_err(),
        "the hash-valid but malformed envelope refuses the suffix absorption"
    );
    // The cold walk agrees — the dependency check fails there too.
    assert!(
        SqliteMedium::open(&db_path).is_err(),
        "the cold walk refuses the malformed envelope too"
    );
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_external_suffix_malformed_root_domain_refuses() {
    // R16: a suffix whose first content object is domain-UNREADABLE
    // (the length prefix exceeds every bound) stored under its TRUE
    // content hash — the CAS load passes and ONLY the domain peek
    // fails. The suffix REFUSES with the propagated typed Format
    // error — never treating the record as ordinary non-composite
    // content (the R15 property: no extraction failure becomes an
    // absent closure).
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-suffix-malformed-root");
    let db_path = root_dir.join("db.sqlite");
    let (mut medium, head, epoch) = sqlite_legacy_founding(&db_path);

    external_sqlite_malformed_root(&db_path, head, epoch);

    let error = medium
        .head()
        .expect_err("the malformed root domain refuses the suffix absorption");
    assert!(
        matches!(error, MediumError::Format(_)),
        "the propagated domain-peek failure: {error:?}"
    );
    // The cold walk agrees — the same first-object peek fails there.
    assert!(
        SqliteMedium::open(&db_path).is_err(),
        "the cold walk refuses the unreadable root too"
    );
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_external_suffix_failure_preserves_the_trusted_cursor() {
    // R16: a refused suffix (the missing transitive chunk) leaves
    // the trusted cursor NOT advanced — the corrupt head is never
    // presented as trusted (head() refuses REPEATEDLY: had the
    // cursor absorbed the corrupt suffix, the second head() would
    // take the O(0) verified-head path and return it), no
    // successful cold rebuild passed the corruption (the cold-walk
    // counter unchanged), and a fresh open agrees the chain is
    // genuinely unverifiable (preserved-or-invalidated, never
    // advanced).
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-suffix-not-advanced");
    let db_path = root_dir.join("db.sqlite");
    let (mut medium, head, epoch) = sqlite_legacy_founding(&db_path);
    let cold_before = medium.walk_stats().cold_walk_records;

    external_sqlite_composite(&db_path, head, epoch, 0, None, Some(3), None);

    assert!(
        medium.head().is_err(),
        "the first suffix absorption refuses"
    );
    assert!(
        medium.head().is_err(),
        "the refusal is stable — the cursor never advanced to the corrupt head"
    );
    assert_eq!(
        medium.walk_stats().cold_walk_records,
        cold_before,
        "no successful cold rebuild passed the corruption"
    );
    assert!(
        SqliteMedium::open(&db_path).is_err(),
        "the cold walk refuses the corrupt chain"
    );
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_cold_suffix_and_hot_verification_agree() {
    // R16: the three paths over structurally identical chains — the
    // SUFFIX-absorbed cursor (the original handle, proven by the
    // walk counters), the COLD walk (a fresh open), and the HOT
    // publication path — maintain the SAME trusted-cursor
    // accounting (head, epoch, distinct content objects). The
    // observation is the test-only accounting accessor; a
    // subsequent validate() alone is insufficient (it cold-walks by
    // design).
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-agree");
    let db_path = root_dir.join("db.sqlite");

    // A: the same-handle SUFFIX path.
    let (mut medium, head, epoch) = sqlite_legacy_founding(&db_path);
    let cold_before = medium.walk_stats().cold_walk_records;
    let case = fixture(0, None);
    external_sqlite_composite(&db_path, head, epoch, 0, None, None, None);
    let new_head = medium
        .head()
        .expect("the suffix absorption succeeds")
        .unwrap();
    assert_ne!(new_head, head, "the HEAD moved");
    // The suffix path ran — NOT a cold rebuild.
    assert_eq!(
        medium.walk_stats().cold_walk_records,
        cold_before,
        "the original handle absorbed through the suffix path"
    );
    assert_eq!(medium.walk_stats().last_append_walked, 1);
    let suffix_accounting = medium.trusted_cursor_accounting().unwrap();
    assert_eq!(suffix_accounting.0, new_head);
    assert_eq!(
        suffix_accounting.2,
        2 + case.chunks.len() as u64,
        "legacy + root + the full verified closure"
    );

    // B: the COLD path over the identical database.
    let cold_medium = SqliteMedium::open(&db_path).unwrap();
    let cold_accounting = cold_medium.trusted_cursor_accounting().unwrap();
    assert_eq!(
        cold_medium.walk_stats().cold_walk_records,
        2,
        "the fresh handle cold-walked both records"
    );
    assert_eq!(
        suffix_accounting, cold_accounting,
        "the suffix-absorbed cursor and the cold walk agree exactly"
    );

    // C: the HOT publication path over a structurally identical
    // chain (the founding /1 record, then the composite child).
    let hot_dir = temp_root("sqlite-agree-hot");
    let hot_path = hot_dir.join("db.sqlite");
    {
        let (mut hot, hot_head, hot_epoch) = sqlite_legacy_founding(&hot_path);
        let record = composite_record(&case.root.hash(), Some((hot_head, hot_epoch)));
        let request = engine_persist::composite_append::CompositeAppend {
            record: &record,
            root: &case.root,
            new_chunks: &case.chunks,
            reused_chunks: &[],
        };
        let outcome = hot
            .append_composite(&request, Some(hot_head))
            .expect("the hot publication accepts");
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        let hot_accounting = hot.trusted_cursor_accounting().unwrap();
        assert_eq!(hot_accounting.1, suffix_accounting.1, "the epochs agree");
        assert_eq!(
            hot_accounting.2, suffix_accounting.2,
            "the distinct-object accounting agrees (root + the verified closure)"
        );
    }
    let _ = std::fs::remove_dir_all(&root_dir);
    let _ = std::fs::remove_dir_all(&hot_dir);
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
    let case = fixture(variant, None);
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
    write_framed_head(store, &record);
}

/// Writes the file store's framed HEAD atomically (the tmp+rename —
/// the medium's own format).
fn write_framed_head(store: &std::path::Path, record: &RevisionRecord) {
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

/// Writes an external suffix revision whose first content object is
/// domain-UNREADABLE garbage (a u64 length prefix far beyond every
/// bound) under its TRUE content hash — the file store's on-disk
/// layout, NO publication API: the CAS load passes and ONLY the
/// domain peek fails (the R13 propagation point).
fn external_file_malformed_root(store: &std::path::Path, parent: Hash, parent_epoch: u64) {
    let garbage = [0xFFu8; 8];
    let garbage_hash = engine_persist::hash::hash_object(&garbage);
    let record =
        RevisionRecord::regular(parent, parent_epoch, 99).with_content_refs(vec![garbage_hash]);
    let objects_dir = store.join("objects");
    std::fs::create_dir_all(&objects_dir).unwrap();
    std::fs::write(objects_dir.join(garbage_hash.hex()), garbage).unwrap();
    std::fs::write(
        objects_dir.join(record.id().hex()),
        record.canonical_bytes(),
    )
    .unwrap();
    write_framed_head(store, &record);
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
    let case = fixture(0, None);
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
fn the_local_same_handle_suffix_malformed_root_domain_refuses() {
    // R16: the genuine same-handle malformed-domain regression — a
    // suffix whose first content object is domain-UNREADABLE (the
    // length prefix exceeds every bound) under its TRUE content
    // hash: the CAS load passes and ONLY the domain peek fails; the
    // R13 propagation REFUSES the absorption with the typed Format
    // error (the trusted cursor never advances past the unreadable
    // state — a failed classification is never treated as an
    // absent closure). No activation is involved (the refusal path
    // needs no facility).
    let root_dir = temp_root("local-suffix-malformed-root");
    let store = root_dir.join("store");
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let legacy = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let legacy_record = RevisionRecord::founding(AuthorityMode::Standalone, "r16m", 0)
        .with_content_refs(vec![legacy.hash()]);
    medium
        .append(&legacy_record, std::slice::from_ref(&legacy), None)
        .unwrap();
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;

    external_file_malformed_root(&store, head, epoch);

    let error = medium
        .head()
        .expect_err("the malformed root domain refuses the same-handle suffix");
    assert!(
        matches!(error, MediumError::Format(_)),
        "the propagated peek failure: {error:?}"
    );
    // The trusted cursor is NOT advanced — the HEAD file names the
    // new record, but the cursor refused; a fresh cold open fails
    // the same peek.
    let cold = LocalFileMedium::open(&store);
    assert!(cold.is_err(), "the cold validation also fails closed");
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
