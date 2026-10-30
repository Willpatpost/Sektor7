//! The R10/R11 regressions — the suffix-absorption error
//! propagation (the trusted cursor refuses unverifiable composite
//! state) and the restored unsupported-future-version recovery
//! invariant (the direct-disk fixture: a /3-family object written
//! to the store WITHOUT any publication API — the recovery must
//! fail closed, never silently fall back to the older /1).

use engine_core::{PresenceSample, Spine};
use engine_persist::T1Journal;
use engine_persist::composite_manifest::{self, CompositeManifest, ManifestEntry, SectionKind};
use engine_persist::local::LocalFileMedium;
use engine_persist::medium::{AppendOutcome, CanonicalMedium};
use engine_persist::record::{AuthorityMode, RevisionRecord};
use engine_persist::{Hash, StoredObject};
use engine_slice::crossing::{SessionCrossing, TransitionError};
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};
use std::path::PathBuf;

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "s7-k2c1-safety-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        TestDir(root)
    }

    fn store(&self) -> PathBuf {
        self.0.join("store")
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.t1")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn near_region_4() -> PresenceSample {
    PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    }
}

fn scenario_pair(ticks: u64) -> (Spine<NpcWorld>, RegionDirector) {
    let config = NpcConfig {
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
    install_initial_schedules(&mut spine, &config);
    let mut director = RegionDirector::new(RegionConfig::default()).unwrap();
    for _ in 0..ticks {
        director.apply(&mut spine, std::slice::from_ref(&near_region_4()));
        spine.run_tick();
    }
    (spine, director)
}

// ---------------------------------------------------- R10: the suffix tests

fn composite_fixture(variant: u8) -> (StoredObject, Vec<StoredObject>) {
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
    (root, chunks)
}

/// Publishes a valid composite through the file medium with the
/// test facility, returning the head and the fixture.
fn publish_composite(
    store: &std::path::Path,
    variant: u8,
) -> (Hash, StoredObject, Vec<StoredObject>) {
    engine_persist::composite_append::test_enable_composite_publication();
    let mut medium = LocalFileMedium::open(store).unwrap();
    let (root, chunks) = composite_fixture(variant);
    let record = RevisionRecord::founding(AuthorityMode::Standalone, "r10", 0)
        .with_content_refs(vec![root.hash()]);
    let request = engine_persist::composite_append::CompositeAppend {
        record: &record,
        root: &root,
        new_chunks: &chunks,
        reused_chunks: &[],
    };
    let outcome = medium.append_composite(&request, None).unwrap();
    assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
    (record.id(), root, chunks)
}

/// Simulates an EXTERNAL append (the suffix scenario): a second
/// process publishes a composite revision on top of the current
/// head, writing the objects + the record + the HEAD file directly
/// (the file medium's on-disk layout — no publication API).
fn external_composite_append(
    store: &std::path::Path,
    parent: Hash,
    parent_epoch: u64,
    variant: u8,
    corrupt_chunk: Option<usize>,
    remove_chunk: Option<usize>,
) -> Result<(), std::io::Result<()>> {
    let (root, chunks) = composite_fixture(variant);
    let mut objects: Vec<StoredObject> = vec![root.clone()];
    objects.extend(chunks.iter().cloned());

    // Write the objects to the objects dir (the insert-once CAS
    // layout: one file per identity).
    let objects_dir = store.join("objects");
    std::fs::create_dir_all(&objects_dir).unwrap();
    for object in &objects {
        let path = objects_dir.join(object.hash().hex());
        if !path.exists() {
            std::fs::write(&path, object.bytes()).unwrap();
        }
    }
    // The corruption / removal injection.
    if let Some(index) = remove_chunk {
        let path = objects_dir.join(chunks[index].hash().hex());
        std::fs::remove_file(&path).map_err(Err)?;
    }
    if let Some(index) = corrupt_chunk {
        let path = objects_dir.join(chunks[index].hash().hex());
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
    }

    // The record.
    let record =
        RevisionRecord::regular(parent, parent_epoch, 99).with_content_refs(vec![root.hash()]);
    let record_path = objects_dir.join(record.id().hex());
    std::fs::write(&record_path, record.canonical_bytes()).unwrap();

    // The HEAD file (the atomic replacement: the framed
    // domain+version+hash object, then tmp + rename — the file
    // medium's own format).
    let head_path = store.join("HEAD");
    let tmp_path = store.join("HEAD.tmp");
    let head_bytes = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str("sektor7/engine-persist/head/1");
        enc.write_u32(1);
        enc.write_bytes(record.id().as_bytes());
        enc.into_bytes()
    };
    std::fs::write(&tmp_path, head_bytes).unwrap();
    std::fs::rename(&tmp_path, &head_path).unwrap();
    Ok(())
}

#[test]
fn the_valid_external_composite_suffix_merges_correctly() {
    // R10 test 1: a valid externally appended composite suffix
    // merges into the trusted cursor (the closure joins the
    // reachable set).
    let dir = TestDir::new("suffix-valid");
    let (head, _root, _chunks) = publish_composite(&dir.store(), 0);
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;
    drop(medium);

    // An external composite append (a different fixture variant so
    // the identities differ).
    external_composite_append(&dir.store(), head, epoch, 60, None, None)
        .map_err(|inner| inner.unwrap_err())
        .unwrap();

    // The reopen: the suffix absorption must verify the closure and
    // succeed (the head is the external record; the chain
    // validates).
    let mut medium =
        LocalFileMedium::open(dir.store()).expect("the valid composite suffix absorbs");
    medium
        .validate()
        .expect("the chain validates with the closure");
    let _ = std::fs::remove_dir_all(&dir.0);
}

#[test]
fn the_missing_transitive_chunk_refuses_suffix_absorption() {
    // R10 test 2: a suffix whose composite root declares a chunk
    // that is absent — the absorption refuses (the trusted cursor
    // does NOT advance past unverifiable state).
    let dir = TestDir::new("suffix-missing");
    let (head, _root, _chunks) = publish_composite(&dir.store(), 0);
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;
    drop(medium);

    // The external append with one chunk REMOVED from disk.
    let result = external_composite_append(&dir.store(), head, epoch, 60, None, Some(3));
    // The append itself succeeds (it writes then removes); the
    // REFUSAL is at the reopen.
    match result {
        Ok(()) => {}
        Err(inner) => {
            let _ = inner;
        }
    }

    let reopen = LocalFileMedium::open(dir.store());
    assert!(
        reopen.is_err(),
        "the missing transitive chunk refuses the suffix absorption (fail closed)"
    );
    let _ = std::fs::remove_dir_all(&dir.0);
}

#[test]
fn the_corrupt_transitive_chunk_refuses_suffix_absorption() {
    // R10 test 3: a suffix whose chunk is corrupted on disk — the
    // absorption refuses.
    let dir = TestDir::new("suffix-corrupt");
    let (head, _root, _chunks) = publish_composite(&dir.store(), 0);
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;
    drop(medium);

    external_composite_append(&dir.store(), head, epoch, 60, Some(3), None)
        .map_err(|inner| {
            let _ = inner;
        })
        .unwrap();

    let reopen = LocalFileMedium::open(dir.store());
    assert!(
        reopen.is_err(),
        "the corrupt transitive chunk refuses the suffix absorption (fail closed)"
    );
    let _ = std::fs::remove_dir_all(&dir.0);
}

// ------------------------------------------ R11: the recovery regression

#[test]
fn the_unsupported_future_version_snapshot_fails_recovery_closed() {
    // R11: the RESTORED original recovery invariant — a newest,
    // recognizable but unsupported session-snapshot family member
    // constructed DIRECTLY on disk (no publication API — the test
    // writes the object + record + HEAD by hand): the production
    // recovery selector REFUSES the newest unsupported snapshot
    // (never silently selecting the older /1).
    let dir = TestDir::new("recovery-future");
    let (spine, director) = scenario_pair(150);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "r11-recovery").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    drop(crossing);

    // The valid /1 snapshot's head + epoch (the recovery baseline).
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;
    drop(medium);

    // The DIRECT-DISK fixture: a /3-family object written to the
    // objects dir + a record referencing it + the HEAD moved to it.
    // NO publication API is involved (the generic append's domain
    // gate would refuse it — this fixture bypasses the API by
    // writing the store's on-disk layout directly, exactly what a
    // pre-K2-C1 store or a foreign tool could produce).
    let future_domain = "engine-slice/session-snapshot/3";
    let mut future_bytes = Vec::new();
    future_bytes.extend_from_slice(&(future_domain.len() as u64).to_le_bytes());
    future_bytes.extend_from_slice(future_domain.as_bytes());
    future_bytes.extend_from_slice(b"future-version payload");
    let future_object = StoredObject::new(future_bytes).unwrap();
    let future_hash = future_object.hash();

    let objects_dir = dir.store().join("objects");
    std::fs::create_dir_all(&objects_dir).unwrap();
    std::fs::write(objects_dir.join(future_hash.hex()), future_object.bytes()).unwrap();
    let future_record =
        RevisionRecord::regular(head, epoch, 999).with_content_refs(vec![future_hash]);
    std::fs::write(
        objects_dir.join(future_record.id().hex()),
        future_record.canonical_bytes(),
    )
    .unwrap();
    let head_path = dir.store().join("HEAD");
    let tmp_path = dir.store().join("HEAD.tmp");
    let head_bytes = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str("sektor7/engine-persist/head/1");
        enc.write_u32(1);
        enc.write_bytes(future_record.id().as_bytes());
        enc.into_bytes()
    };
    std::fs::write(&tmp_path, head_bytes).unwrap();
    std::fs::rename(&tmp_path, &head_path).unwrap();

    // THE PRODUCTION RECOVERY: must FAIL CLOSED on the unsupported
    // newest snapshot — never silently selecting the older /1.
    let error = match SessionCrossing::recover(&dir.store(), &dir.journal(), "r11-recovery") {
        Err(error) => error,
        Ok(_) => panic!("the future-version snapshot fails the recovery closed"),
    };
    assert!(
        matches!(error, TransitionError::CorruptSnapshot(_)),
        "the typed fail-closed class: {error:?}"
    );
    let _ = std::fs::remove_dir_all(&dir.0);
}
