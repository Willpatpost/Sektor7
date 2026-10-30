//! The K2-C2A PUBLICATION FENCE regressions — the provider-side
//! reused-dependency revalidation under the coordination boundary.
//! The deterministic admission-to-publication race: the caller-side
//! admission PASSES (every reused dependency verified present), the
//! dependency then vanishes or changes at the race's midpoint (the
//! sweep's quarantine action, a committed reclamation delete, or
//! on-disk corruption), and the publication REFUSES typed inside
//! the boundary — nothing mutated, the HEAD unchanged, the new
//! chunks unwritten. The worker test proves the fence survives the
//! canonical I/O queue. All run with the TEST-ONLY activation
//! facility — production never enables it.
//!
//! The midpoint vanishings here are the deterministic minimal forms
//! of the production race: the quarantine move IS the session-open
//! sweep's own action (the exact rename + reason-note sequence
//! `sweep_unreferenced` performs), and the plain deletion is a
//! committed Phase B delete. The full end-to-end sweep race over a
//! genuinely compacted chain (an already-reclaimed range whose
//! chunks the successor reuses) belongs to K2-C2B's compacted-
//! history matrix — the fence's contract is identical.

use engine_persist::composite_append::{self, CompositeAppend};
use engine_persist::composite_manifest::{self, CompositeManifest, ManifestEntry, SectionKind};
use engine_persist::local::LocalFileMedium;
use engine_persist::medium::{AppendOutcome, CanonicalMedium, MediumError};
use engine_persist::record::{AuthorityMode, RevisionRecord};
use engine_persist::sqlite::SqliteMedium;
use engine_persist::{Hash, StoredObject};
use engine_slice::store::{MediumSource, StoreLocation};
use engine_slice::worker::IoWorker;

fn temp_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "s7-k2c2a-{name}-{}-{}",
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
    entries: Vec<ManifestEntry>,
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
    seed = seed.wrapping_add(1);
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
    Fixture {
        root,
        chunks,
        entries: manifest.entries,
    }
}

/// The successor snapshot: the six singleton sections are REUSED
/// from the base (identical identities — the unchanged sections of
/// an incremental snapshot), the ledger run and the tail sections
/// are new.
fn successor_fixture(base: &Fixture, variant: u8) -> Fixture {
    let mut seed = variant;
    let mut entries: Vec<ManifestEntry> = base.entries[0..6].to_vec();
    let mut chunks = Vec::new();
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
    seed = seed.wrapping_add(1);
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
        let payload = vec![seed; 10 + seed as usize];
        seed = seed.wrapping_add(1);
        let object = composite_manifest::encode_chunk_envelope(tag, &payload);
        entries.push(ManifestEntry {
            tag,
            identity: object.hash(),
            payload_length: payload.len() as u64,
            kind: SectionKind::Singleton,
        });
        chunks.push(object);
    }
    let manifest = CompositeManifest {
        history_kind: composite_manifest::HistoryKind::Full,
        ledger_boundary: 4,
        composed_length: entries.iter().map(|e| e.payload_length).sum(),
        entries,
    };
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    Fixture {
        root,
        chunks,
        entries: manifest.entries,
    }
}

/// The founding composite publication (the base snapshot, all-new
/// chunks) through `append_composite` — returns the medium's trusted
/// head and epoch for the successor's parent binding.
fn publish_founding<M: CanonicalMedium>(medium: &mut M, base: &Fixture) -> (Hash, u64, Vec<Hash>) {
    let record = RevisionRecord::founding(AuthorityMode::Standalone, "k2c2a", 0)
        .with_content_refs(vec![base.root.hash()]);
    let request = CompositeAppend {
        record: &record,
        root: &base.root,
        new_chunks: &base.chunks,
        reused_chunks: &[],
    };
    let outcome = medium
        .append_composite(&request, None)
        .expect("the founding composite publication accepts");
    assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;
    let reused: Vec<Hash> = base.chunks[0..6].iter().map(|c| c.hash()).collect();
    (head, epoch, reused)
}

/// The successor's publication request (the root-only record over
/// the successor root, the new ledger chunks supplied, the base's
/// singleton sections reused). The record is caller-owned — the
/// request borrows it.
fn successor_request<'a>(
    record: &'a RevisionRecord,
    successor: &'a Fixture,
    reused: &'a [Hash],
) -> CompositeAppend<'a> {
    CompositeAppend {
        record,
        root: &successor.root,
        new_chunks: &successor.chunks,
        reused_chunks: reused,
    }
}

fn successor_record(successor: &Fixture, head: Hash, epoch: u64) -> RevisionRecord {
    RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![successor.root.hash()])
}

// ------------------------------------------------------ the local provider

#[test]
fn the_local_fence_accepts_when_every_dependency_remains() {
    // The positive control: the reused dependencies are present at
    // publication — the admission AND the fence both pass, the
    // successor is accepted, the chain validates, and a reused
    // chunk reads back byte-identical through the same handle.
    composite_append::test_enable_composite_publication();
    let root_dir = temp_root("local-positive");
    let store = root_dir.join("store");
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let base = fixture(0);
    let (head, epoch, reused) = publish_founding(&mut medium, &base);

    let successor = successor_fixture(&base, 20);
    let record = successor_record(&successor, head, epoch);
    let request = successor_request(&record, &successor, &reused);
    let outcome = medium
        .append_composite(&request, Some(head))
        .expect("the successor publication accepts");
    assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
    assert_eq!(medium.head().unwrap().unwrap(), record.id());
    medium.validate().expect("the chain validates");
    let bytes = medium.read_object(&reused[2]).unwrap();
    assert_eq!(bytes, base.chunks[2].bytes());
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_local_fence_refuses_the_sweep_quarantine_between_admission_and_publication() {
    // THE K2-C2A race: the caller-side admission PASSES (every
    // reused dependency read and verified), the sweep's own action
    // (the quarantine rename + reason note) removes a chunk at the
    // race's midpoint, and the publication REFUSES inside HEAD.lock
    // — nothing mutated: the HEAD unchanged, the successor's new
    // chunks unwritten, the evidence preserved in quarantine.
    composite_append::test_enable_composite_publication();
    let root_dir = temp_root("local-sweep-race");
    let store = root_dir.join("store");
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let base = fixture(0);
    let (head, epoch, reused) = publish_founding(&mut medium, &base);

    let successor = successor_fixture(&base, 20);
    let record = successor_record(&successor, head, epoch);
    let request = successor_request(&record, &successor, &reused);
    // The caller-side admission over the SAME handle: it PASSES —
    // every reused dependency is present and verified.
    let receipt =
        composite_append::admit_for_publication(&request, |identity| medium.read_object(identity))
            .expect("the caller-side admission passes (the dependencies are present)");

    // The sweep's action at the race's midpoint — the exact
    // rename + reason-note sequence `sweep_unreferenced` performs
    // (objects/<hex> → quarantine/<hex> + the .reason note).
    let vanished = reused[2];
    let objects_dir = store.join("objects");
    let quarantine_dir = store.join("quarantine");
    std::fs::create_dir_all(&quarantine_dir).unwrap();
    std::fs::rename(
        objects_dir.join(vanished.hex()),
        quarantine_dir.join(vanished.hex()),
    )
    .unwrap();
    std::fs::write(
        quarantine_dir.join(format!("{}.reason", vanished.hex())),
        "sweep-unreferenced",
    )
    .unwrap();

    // The publication REFUSES at the fence — the typed
    // identity-naming absence, never a silent acceptance.
    let refusal = medium
        .append_admitted(receipt, Some(head))
        .expect_err("the fence refuses the vanished dependency");
    assert!(
        matches!(refusal, MediumError::MissingObject { hash } if hash == vanished),
        "the typed refusal: {refusal:?}"
    );
    // Nothing mutated: the HEAD is unchanged (the trusted cursor
    // still matches it — the O(0) path) and the successor's new
    // chunks were never written.
    assert_eq!(medium.head().unwrap().unwrap(), head);
    for chunk in &successor.chunks {
        assert!(
            !objects_dir.join(chunk.hash().hex()).exists(),
            "the successor's new chunk was never written"
        );
    }
    // The evidence is preserved in quarantine.
    assert!(quarantine_dir.join(vanished.hex()).is_file());
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_local_fence_refuses_the_deleted_dependency_between_admission_and_publication() {
    // The Phase B form of the race: a committed reclamation delete
    // removes the chunk between the admission and the publication.
    composite_append::test_enable_composite_publication();
    let root_dir = temp_root("local-delete-race");
    let store = root_dir.join("store");
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let base = fixture(0);
    let (head, epoch, reused) = publish_founding(&mut medium, &base);

    let successor = successor_fixture(&base, 20);
    let record = successor_record(&successor, head, epoch);
    let request = successor_request(&record, &successor, &reused);
    let receipt =
        composite_append::admit_for_publication(&request, |identity| medium.read_object(identity))
            .expect("the caller-side admission passes");

    let vanished = reused[4];
    std::fs::remove_file(store.join("objects").join(vanished.hex())).unwrap();

    let refusal = medium
        .append_admitted(receipt, Some(head))
        .expect_err("the fence refuses the deleted dependency");
    assert!(
        matches!(refusal, MediumError::MissingObject { hash } if hash == vanished),
        "the typed refusal: {refusal:?}"
    );
    assert_eq!(medium.head().unwrap().unwrap(), head);
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_local_fence_refuses_the_corrupted_dependency_between_admission_and_publication() {
    // The corruption form: the chunk's bytes change after the
    // admission — the fence's hash-verified load refuses with the
    // referenced-corruption class.
    composite_append::test_enable_composite_publication();
    let root_dir = temp_root("local-corrupt-race");
    let store = root_dir.join("store");
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let base = fixture(0);
    let (head, epoch, reused) = publish_founding(&mut medium, &base);

    let successor = successor_fixture(&base, 20);
    let record = successor_record(&successor, head, epoch);
    let request = successor_request(&record, &successor, &reused);
    let receipt =
        composite_append::admit_for_publication(&request, |identity| medium.read_object(identity))
            .expect("the caller-side admission passes");

    let corrupted = reused[1];
    let path = store.join("objects").join(corrupted.hex());
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();

    let refusal = medium
        .append_admitted(receipt, Some(head))
        .expect_err("the fence refuses the corrupted dependency");
    assert!(
        matches!(refusal, MediumError::ReferencedObjectCorrupt { hash } if hash == corrupted),
        "the typed refusal: {refusal:?}"
    );
    assert_eq!(medium.head().unwrap().unwrap(), head);
    let _ = std::fs::remove_dir_all(&root_dir);
}

// ---------------------------------------------------- the SQLite provider

#[test]
fn the_sqlite_fence_accepts_when_every_dependency_remains() {
    // The positive control on the production provider: the reused
    // dependencies are present — the admission AND the in-transaction
    // fence both pass, the successor is accepted, the chain
    // validates.
    composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-positive");
    let db_path = root_dir.join("db.sqlite");
    let mut medium = SqliteMedium::open(&db_path).unwrap();
    let base = fixture(0);
    let (head, epoch, reused) = publish_founding(&mut medium, &base);

    let successor = successor_fixture(&base, 20);
    let record = successor_record(&successor, head, epoch);
    let request = successor_request(&record, &successor, &reused);
    let outcome = medium
        .append_composite(&request, Some(head))
        .expect("the successor publication accepts");
    assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
    assert_eq!(medium.head().unwrap().unwrap(), record.id());
    medium.validate().expect("the chain validates");
    let bytes = medium.read_object(&reused[2]).unwrap();
    assert_eq!(bytes, base.chunks[2].bytes());
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_fence_refuses_the_deleted_row_between_admission_and_publication() {
    // The SQLite race: the caller-side admission PASSES, a second
    // connection commits the reclamation delete of a reused chunk
    // row, and the publication REFUSES inside the write transaction
    // — the HEAD unchanged, the successor's rows never inserted.
    composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-delete-race");
    let db_path = root_dir.join("db.sqlite");
    let mut medium = SqliteMedium::open(&db_path).unwrap();
    let base = fixture(0);
    let (head, epoch, reused) = publish_founding(&mut medium, &base);

    let successor = successor_fixture(&base, 20);
    let record = successor_record(&successor, head, epoch);
    let request = successor_request(&record, &successor, &reused);
    let receipt =
        composite_append::admit_for_publication(&request, |identity| medium.read_object(identity))
            .expect("the caller-side admission passes");

    // The committed delete through a second connection (the
    // production provider's own table).
    let vanished = reused[3];
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "DELETE FROM objects WHERE hash = ?1",
        rusqlite::params![vanished.as_bytes()],
    )
    .unwrap();
    drop(conn);

    let refusal = medium
        .append_admitted(receipt, Some(head))
        .expect_err("the fence refuses the deleted row");
    assert!(
        matches!(refusal, MediumError::MissingObject { hash } if hash == vanished),
        "the typed refusal: {refusal:?}"
    );
    assert_eq!(medium.head().unwrap().unwrap(), head);
    // The successor's rows were never inserted (the transaction
    // rolled back before the inserts).
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    for chunk in &successor.chunks {
        let present: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE hash = ?1)",
                rusqlite::params![chunk.hash().as_bytes()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(present, 0, "the successor's new chunk was never inserted");
    }
    let _ = std::fs::remove_dir_all(&root_dir);
}

// ------------------------------------------------------- the worker queue

#[test]
fn the_worker_transport_relays_the_fence_refusal() {
    // The fence survives the canonical I/O queue: the receipt is
    // admitted caller-side (the reads through the queue PASS), the
    // dependency vanishes at the race's midpoint, and the
    // AdmittedAppend job's fence refusal relays back through the
    // transport as the typed error — the store unchanged.
    composite_append::test_enable_composite_publication();
    let root_dir = temp_root("worker-race");
    let db_path = root_dir.join("db.sqlite");
    let location = StoreLocation::Database(db_path.clone());
    let worker = IoWorker::spawn(&location).unwrap();
    let mut proxy = worker.source().unwrap().open_medium().unwrap();

    // The founding publication THROUGH the worker (the admission
    // reads and the append all ride the queue).
    let base = fixture(0);
    let (head, epoch, reused) = publish_founding(&mut proxy, &base);

    let successor = successor_fixture(&base, 20);
    let record = successor_record(&successor, head, epoch);
    let request = successor_request(&record, &successor, &reused);
    // The caller-side admission through the SAME queue proxy: it
    // PASSES (every reused dependency read and verified through the
    // worker's single connection).
    let receipt =
        composite_append::admit_for_publication(&request, |identity| proxy.read_object(identity))
            .expect("the caller-side admission passes through the queue");

    // The committed delete through a second connection.
    let vanished = reused[5];
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "DELETE FROM objects WHERE hash = ?1",
        rusqlite::params![vanished.as_bytes()],
    )
    .unwrap();
    drop(conn);

    // The append job: the worker executes the provider's
    // append_admitted; the fence refuses inside the transaction;
    // the typed error relays through the reply slot.
    let refusal = proxy
        .append_admitted(receipt, Some(head))
        .expect_err("the fence refusal relays through the worker transport");
    assert!(
        matches!(refusal, MediumError::MissingObject { hash } if hash == vanished),
        "the typed refusal: {refusal:?}"
    );
    assert_eq!(proxy.head().unwrap().unwrap(), head);
    let _ = std::fs::remove_dir_all(&root_dir);
}
