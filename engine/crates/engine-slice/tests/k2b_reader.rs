//! The S6-C4 Phase 2 K2-B dark reader's ENGINE-SLICE INTEGRATION —
//! the real corpus end to end: the walked composite sessions
//! published into isolated mock stores, read back through the dark
//! dependency-closure reader, reconstructed to the EXACT legacy
//! bytes, and passed through the UNCHANGED strict restoration
//! contract (the sole authority over global session validity —
//! hash-valid chunks are never equated with a globally valid
//! session). Both history forms, the deterministic continuation
//! equivalence, the typed negatives (missing, corrupt, mis-tagged,
//! truncated, lying manifests, future versions), the memo's
//! equivalence over the REAL shared sealed prefix, and the pod-only
//! performance model. No production path depends on any of this.

use std::collections::HashMap;
use std::path::PathBuf;

use engine_core::{PresenceSample, Spine};
use engine_persist::composite_manifest::{self, TAG_FAR, TAG_NEAR};
use engine_persist::composite_reader::{
    self, CompositeReadError, ObjectSource, ObjectSourceError, ReadLimits, VerificationMemo,
};
use engine_persist::dark_reachability::{self, ReachabilityLimits};
use engine_persist::record::{AuthorityMode, RevisionRecord};
use engine_persist::{Hash, StoredObject, T1Journal};
use engine_slice::crossing::{SessionCrossing, restore_session, save_session};
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};
use engine_slice::session_chunks::{self, ChunkPolicy, HistoryForm, SectionChunk};

// ------------------------------------------------------------- fixtures

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-k2b-reader-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
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

/// The real scenario pair (the crossing suite's fixture).
fn scenario_pair(ticks: u64, presence: &PresenceSample) -> (Spine<NpcWorld>, RegionDirector) {
    let config = NpcConfig {
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
    install_initial_schedules(&mut spine, &config);
    let mut director = RegionDirector::new(RegionConfig::default()).unwrap();
    for _ in 0..ticks {
        director.apply(&mut spine, std::slice::from_ref(presence));
        spine.run_tick();
    }
    (spine, director)
}

/// An isolated mock store over an identity-keyed map, counting every
/// read (the memo's verification-cost witness).
struct MapSource {
    objects: HashMap<Hash, Vec<u8>>,
    reads: usize,
}

impl MapSource {
    fn new() -> Self {
        MapSource {
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

impl ObjectSource for MapSource {
    fn read_object(&mut self, identity: &Hash) -> Result<Vec<u8>, ObjectSourceError> {
        self.reads += 1;
        self.objects
            .get(identity)
            .cloned()
            .ok_or(ObjectSourceError::Missing(*identity))
    }
}

/// Publishes one walked session into an isolated mock store: every
/// section's chunk envelope under its content identity, plus the
/// ratified root object. Returns the store and the root.
fn composite_store(chunks: &session_chunks::ChunkedSession) -> (MapSource, StoredObject) {
    let root = StoredObject::new(session_chunks::try_root_manifest(chunks).unwrap()).unwrap();
    let mut source = MapSource::new();
    for section in chunks.section_list() {
        let envelope = composite_manifest::encode_chunk_envelope(section.tag, &section.payload);
        source.insert(envelope.hash(), envelope.into_bytes());
    }
    (source, root)
}

fn read_session(chunks: &session_chunks::ChunkedSession) -> composite_reader::VerifiedComposite {
    let (mut source, root) = composite_store(chunks);
    composite_reader::read_composite_closure(&mut source, &root, &ReadLimits::default())
        .unwrap_or_else(|error| panic!("the composite reads: {error:?}"))
}

// --------------------------------------------------- the positive corpus

#[test]
fn the_full_history_composites_read_reconstruct_and_strictly_restore() {
    // Items 1, 14 (the valid full-history composite over the real
    // corpus, including the freshly-initialized zero-record corner):
    // the verified closure, the byte-exact legacy reconstruction,
    // the unchanged strict restoration, and the deterministic
    // continuation equivalence.
    for ticks in [0u64, 5, 150, 600] {
        let (spine, director) = scenario_pair(ticks, &near_region_4());
        let legacy = save_session(&spine, &director).unwrap();
        let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
        let verified = read_session(&chunks);

        // The verified closure matches the walk's own declarations.
        assert_eq!(
            verified.root_identity,
            session_chunks::try_root_identity(&chunks).unwrap()
        );
        assert_eq!(
            verified.verified_identities(),
            chunks
                .section_list()
                .iter()
                .map(|section| section.identity)
                .collect::<Vec<_>>(),
            "ticks {ticks}: the verified order is the walk's section order"
        );
        assert_eq!(
            verified.composed_payloads().len() as u64,
            verified.manifest.composed_length
        );

        // The EXACT legacy reconstruction.
        let reconstructed = session_chunks::reconstruct_legacy_bytes(&verified).unwrap();
        assert_eq!(
            reconstructed, legacy,
            "ticks {ticks}: the verified closure reconstructs the legacy bytes exactly"
        );

        // The unchanged strict restoration over the reconstructed
        // bytes (the global gate — the compact-safe state domain).
        let (mut restored_spine, mut restored_director) = restore_session(&reconstructed).unwrap();
        assert_eq!(restored_spine.clock(), spine.clock());
        assert_eq!(restored_spine.stats(), spine.stats());
        assert_eq!(restored_spine.held_count(), spine.held_count());
        assert_eq!(restored_spine.held_fingerprint(), spine.held_fingerprint());
        assert_eq!(
            restored_spine.world().milestones(),
            spine.world().milestones()
        );
        assert_eq!(restored_director.levels(), director.levels());

        // The deterministic continuation equivalence: BOTH states
        // run the same ticks and behave identically.
        let (mut spine, mut director) = (spine, director);
        let presence = near_region_4();
        for _ in 0..40 {
            director.apply(&mut spine, std::slice::from_ref(&presence));
            spine.run_tick();
            restored_director.apply(&mut restored_spine, std::slice::from_ref(&presence));
            restored_spine.run_tick();
        }
        assert_eq!(spine.stats(), restored_spine.stats(), "ticks {ticks}");
        assert_eq!(spine.clock(), restored_spine.clock());
        assert_eq!(spine.held_fingerprint(), restored_spine.held_fingerprint());
        assert_eq!(director.levels(), restored_director.levels());
    }
}

#[test]
fn the_compacted_history_composite_reads_reconstructs_and_strictly_restores() {
    // Item 2: the valid compacted-history composite — the /2 spine
    // form's compacted-meta section rides the closure, and the
    // reconstruction restores strictly through the same contract.
    let dir = TestDir::new("compacted");
    let (mut spine, mut director) = scenario_pair(600, &near_region_4());
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "k2b-compacted").unwrap();
    crossing
        .compact_session(&mut spine, &mut director, 512)
        .expect("the compaction applies");
    drop(crossing);
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    assert_eq!(chunks.form, HistoryForm::Compacted);
    let verified = read_session(&chunks);
    assert_eq!(
        verified.manifest.history_kind,
        composite_manifest::HistoryKind::Compacted
    );
    let reconstructed = session_chunks::reconstruct_legacy_bytes(&verified).unwrap();
    assert_eq!(reconstructed, legacy);
    let (restored_spine, restored_director) = restore_session(&reconstructed).unwrap();
    assert_eq!(restored_spine.clock(), spine.clock());
    assert_eq!(restored_spine.stats(), spine.stats());
    assert_eq!(restored_director.levels(), director.levels());
}

#[test]
fn the_repeated_reads_are_identical() {
    // Item 14: the repeated verified reads produce the identical
    // closure and the identical reconstruction.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let first = read_session(&chunks);
    let second = read_session(&chunks);
    assert_eq!(first, second);
    assert_eq!(
        session_chunks::reconstruct_legacy_bytes(&first).unwrap(),
        session_chunks::reconstruct_legacy_bytes(&second).unwrap()
    );
}

// --------------------------------------------------------- the negatives

#[test]
fn the_missing_newest_root_chunk_fails_with_its_identity() {
    // Item 5: the newest root's OWN chunk (the director — the final
    // entry) missing from the store fails closed with the identity;
    // a missing sealed ledger chunk behaves identically.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let director_identity = chunks.director.identity;
    let (mut source, root) = composite_store(&chunks);
    source.remove(&director_identity);
    assert_eq!(
        composite_reader::read_composite_closure(&mut source, &root, &ReadLimits::default()),
        Err(CompositeReadError::Missing {
            identity: director_identity
        })
    );
    let sealed_identity = chunks.ledger_chunks[0].identity;
    let (mut source, root) = composite_store(&chunks);
    source.remove(&sealed_identity);
    assert_eq!(
        composite_reader::read_composite_closure(&mut source, &root, &ReadLimits::default()),
        Err(CompositeReadError::Missing {
            identity: sealed_identity
        })
    );
}

#[test]
fn the_corrupt_newest_root_chunk_fails_the_identity_rehash() {
    // Item 6: the newest root's chunk corrupted in place (the
    // envelope's framing, domain, tag, and length intact; a payload
    // byte flipped) — the reader's OWN re-hash catches it.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let director_identity = chunks.director.identity;
    let (mut source, root) = composite_store(&chunks);
    let intact = source.objects[&director_identity].clone();
    let mut corrupted = intact;
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0xff;
    source.insert(director_identity, corrupted);
    assert_eq!(
        composite_reader::read_composite_closure(&mut source, &root, &ReadLimits::default()),
        Err(CompositeReadError::HashMismatch {
            identity: director_identity
        })
    );
}

#[test]
fn the_wrong_envelope_section_tag_fails_before_the_hash() {
    // Item 8: the near entry's identity holding a WELL-FORMED
    // far-tagged envelope — the tag mismatch reports before the
    // (also failing) hash.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let near_identity = chunks.near.identity;
    let near_length = chunks.near.payload.len();
    let (mut source, root) = composite_store(&chunks);
    let mis_tagged = composite_manifest::encode_chunk_envelope(TAG_FAR, &vec![9u8; near_length]);
    source.insert(near_identity, mis_tagged.into_bytes());
    assert_eq!(
        composite_reader::read_composite_closure(&mut source, &root, &ReadLimits::default()),
        Err(CompositeReadError::Tag {
            identity: near_identity,
            expected: TAG_NEAR,
            observed: TAG_FAR.to_owned(),
        })
    );
}

#[test]
fn the_truncated_chunk_fails_typed() {
    // Item 9: a truncated stored envelope (the framing broken).
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let world_identity = chunks.world.identity;
    let (mut source, root) = composite_store(&chunks);
    let intact = source.objects[&world_identity].clone();
    source.insert(world_identity, intact[..intact.len() - 12].to_vec());
    let error =
        composite_reader::read_composite_closure(&mut source, &root, &ReadLimits::default())
            .unwrap_err();
    assert!(
        matches!(
            error,
            CompositeReadError::Envelope { identity, .. } if identity == world_identity
        ),
        "the truncated chunk fails closed: {error:?}"
    );
}

#[test]
fn the_hash_valid_globally_invalid_composite_fails_the_strict_restoration() {
    // Item 10 — THE reader/restoration seam: a composite whose every
    // chunk is envelope-valid and hash-valid (a COHERENT manifest
    // over MANGLED payload bytes — the mangled header re-enveloped
    // and re-identified, the manifest rebuilt over the mangled
    // session) reads SUCCESSFULLY through the dark reader, yet the
    // reconstructed bytes FAIL the strict restoration: hash-valid
    // chunks are never a globally valid session.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    // Mangle the header's save-domain digit ('1' → '0' — the payload
    // is [8-byte length]["sektor7/save/1"]…, the digit at offset 21)
    // and re-envelope it under its NEW identity.
    let mut mangled_payload = chunks.header.payload.clone();
    assert_eq!(mangled_payload[21], b'1', "the save-domain digit's offset");
    mangled_payload[21] = b'0';
    let mangled_envelope =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_HEADER, &mangled_payload);
    let mangled_header = SectionChunk {
        tag: chunks.header.tag,
        payload: mangled_payload,
        identity: mangled_envelope.hash(),
    };
    let mangled = session_chunks::ChunkedSession {
        form: chunks.form,
        header: mangled_header,
        near: chunks.near.clone(),
        far: chunks.far.clone(),
        blocked: chunks.blocked.clone(),
        held: chunks.held.clone(),
        compacted_meta: chunks.compacted_meta.clone(),
        ledger_header: chunks.ledger_header.clone(),
        ledger_boundary: chunks.ledger_boundary,
        ledger_chunks: chunks.ledger_chunks.clone(),
        world: chunks.world.clone(),
        director: chunks.director.clone(),
    };
    // The dark reader ACCEPTS the mangled composite: every chunk is
    // structurally valid, every identity authentic.
    let verified = read_session(&mangled);
    assert_eq!(
        verified.verified_identities()[0],
        mangled_envelope.hash(),
        "the mangled header verifies — the corruption is semantic, not structural"
    );
    // The reconstruction restores NOTHING: the strict restoration
    // fails typed on the mangled save domain.
    let reconstructed = session_chunks::reconstruct_legacy_bytes(&verified).unwrap();
    assert_ne!(reconstructed, legacy);
    let error = match restore_session(&reconstructed) {
        Err(error) => error,
        Ok(_) => panic!("the globally invalid composite fails the strict restoration"),
    };
    assert!(
        matches!(error, engine_core::SaveError::Domain { .. }),
        "the globally invalid composite fails the strict restoration typed: {error:?}"
    );
}

#[test]
fn the_manifest_length_mismatch_and_reordering_fail() {
    // Item 11: (a) a manifest declaring eight more payload bytes for
    // the near chunk (the composed total adjusted — the decode's
    // coherence holds; only the reader's per-entry length check
    // catches the lie); (b) a reordered manifest fails the grammar.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let near_identity = chunks.near.identity;
    let near_length = chunks.near.payload.len() as u64;

    let mut manifest = session_chunks::try_build_manifest(&chunks).unwrap();
    manifest.entries[1].payload_length += 8;
    manifest.composed_length += 8;
    let lying_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let (mut source, _) = composite_store(&chunks);
    assert_eq!(
        composite_reader::read_composite_closure(&mut source, &lying_root, &ReadLimits::default()),
        Err(CompositeReadError::Length {
            identity: near_identity,
            declared: near_length + 8,
            actual: near_length,
        })
    );

    let mut manifest = session_chunks::try_build_manifest(&chunks).unwrap();
    manifest.entries.swap(1, 2);
    let reordered_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let (mut source, _) = composite_store(&chunks);
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
fn the_future_snapshot_version_fails_the_strict_decode() {
    // Item 12: an unsupported future snapshot version — the root's
    // version field patched (the low byte at 8 + 31, the domain
    // being 31 bytes).
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let (_, root) = composite_store(&chunks);
    let mut versioned = root.bytes().to_vec();
    versioned[8 + 31] = 3;
    let future_root = StoredObject::new(versioned).unwrap();
    let (mut source, _) = composite_store(&chunks);
    let error =
        composite_reader::read_composite_closure(&mut source, &future_root, &ReadLimits::default())
            .unwrap_err();
    assert!(matches!(
        error,
        CompositeReadError::Manifest(engine_core::SaveError::Version { observed: 3, .. })
    ));
}

#[test]
fn the_duplicate_and_extraneous_dependency_identities_fail_or_stay_out() {
    // Item 13: (a) two grammatical entries naming the SAME identity
    // fail the strict decode's duplicate rejection; (b) extraneous
    // objects in the store NEVER leak into the verified closure.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();

    let mut manifest = session_chunks::try_build_manifest(&chunks).unwrap();
    manifest.entries[2].identity = manifest.entries[1].identity;
    manifest.entries[2].payload_length = manifest.entries[1].payload_length;
    let duplicate_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let (mut source, _) = composite_store(&chunks);
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

    let (mut source, root) = composite_store(&chunks);
    for seed in 200..203u8 {
        let unrelated = composite_manifest::encode_chunk_envelope(TAG_NEAR, &[seed; 7]);
        source.insert(unrelated.hash(), unrelated.into_bytes());
    }
    let verified =
        composite_reader::read_composite_closure(&mut source, &root, &ReadLimits::default())
            .unwrap();
    assert_eq!(
        verified.dependency_identities(),
        chunks
            .section_list()
            .iter()
            .map(|section| section.identity)
            .collect::<Vec<_>>(),
        "the dependency set is EXACTLY the manifest's declared closure"
    );
}

// ------------------------------------------- the shared prefix and memo

#[test]
fn the_shared_sealed_prefix_and_the_memo_equivalence() {
    // Items 4 and 15 over the REAL corpus: a session and its
    // advanced successor SHARE the sealed ledger prefix (the K1
    // stability invariant); reading both roots with ONE threaded
    // memo verifies each shared chunk ONCE, and the outcomes are
    // IDENTICAL to the fresh-memo reads. A corrupted shared chunk
    // fails BOTH modes identically.
    let (mut spine, mut director) = scenario_pair(600, &near_region_4());
    let first_legacy = save_session(&spine, &director).unwrap();
    let first = session_chunks::chunk_session(&first_legacy, &ChunkPolicy::default()).unwrap();
    let presence = near_region_4();
    for _ in 0..50 {
        director.apply(&mut spine, std::slice::from_ref(&presence));
        spine.run_tick();
    }
    let second_legacy = save_session(&spine, &director).unwrap();
    let second = session_chunks::chunk_session(&second_legacy, &ChunkPolicy::default()).unwrap();
    let sealed_shared = first.sealed_prefix_identities();
    assert!(
        !sealed_shared.is_empty()
            && second.sealed_prefix_identities()[..sealed_shared.len()] == sealed_shared[..],
        "the two roots genuinely share the sealed prefix"
    );
    // The TRUE distinct union (the sealed prefix AND any unchanged
    // singleton sections — the fixture's advanced ticks may leave
    // lanes untouched, and content addressing makes identical
    // payloads the same identity).
    let first_ids: std::collections::BTreeSet<Hash> = first
        .section_list()
        .iter()
        .map(|section| section.identity)
        .collect();
    let second_ids: std::collections::BTreeSet<Hash> = second
        .section_list()
        .iter()
        .map(|section| section.identity)
        .collect();
    let distinct_union = first_ids.union(&second_ids).count();
    let shared = first_ids.intersection(&second_ids).count();
    assert!(shared >= sealed_shared.len());

    let (mut source, first_root) = composite_store(&first);
    let second_root =
        StoredObject::new(session_chunks::try_root_manifest(&second).unwrap()).unwrap();
    for section in second.section_list() {
        let envelope = composite_manifest::encode_chunk_envelope(section.tag, &section.payload);
        source.insert(envelope.hash(), envelope.into_bytes());
    }

    // One threaded memo across both roots: the shared chunks are
    // read and hash-verified ONCE.
    let mut memo = VerificationMemo::new();
    let memo_first = composite_reader::read_composite_closure_with_memo(
        &mut source,
        &first_root,
        &ReadLimits::default(),
        &mut memo,
    )
    .unwrap();
    let memo_second = composite_reader::read_composite_closure_with_memo(
        &mut source,
        &second_root,
        &ReadLimits::default(),
        &mut memo,
    )
    .unwrap();
    assert_eq!(
        memo.len(),
        distinct_union,
        "the memo carries each distinct chunk exactly once"
    );
    assert_eq!(source.reads, distinct_union);

    // Fresh memos (the no-memo behavior): every identity of the
    // second root re-reads — and the results are IDENTICAL.
    let (mut source, first_root) = composite_store(&first);
    let second_root =
        StoredObject::new(session_chunks::try_root_manifest(&second).unwrap()).unwrap();
    for section in second.section_list() {
        let envelope = composite_manifest::encode_chunk_envelope(section.tag, &section.payload);
        source.insert(envelope.hash(), envelope.into_bytes());
    }
    let fresh_first =
        composite_reader::read_composite_closure(&mut source, &first_root, &ReadLimits::default())
            .unwrap();
    let fresh_second =
        composite_reader::read_composite_closure(&mut source, &second_root, &ReadLimits::default())
            .unwrap();
    assert_eq!(
        source.reads,
        first.section_list().len() + second.section_list().len()
    );
    assert_eq!(memo_first, fresh_first);
    assert_eq!(memo_second, fresh_second);
    assert_eq!(
        session_chunks::reconstruct_legacy_bytes(&memo_second).unwrap(),
        second_legacy,
        "the memoized read reconstructs the successor's exact legacy bytes"
    );

    // The negative: a corrupted SHARED chunk fails both modes with
    // the identical typed error, and never earns a memo entry.
    let (mut source, first_root) = composite_store(&first);
    let victim = sealed_shared[0];
    let intact = source.objects[&victim].clone();
    let mut corrupted = intact;
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0xff;
    source.insert(victim, corrupted);
    let mut memo = VerificationMemo::new();
    let memo_error = composite_reader::read_composite_closure_with_memo(
        &mut source,
        &first_root,
        &ReadLimits::default(),
        &mut memo,
    )
    .unwrap_err();
    assert_eq!(
        memo_error,
        CompositeReadError::HashMismatch { identity: victim }
    );
    assert!(!memo.contains(&victim));
    let fresh_error =
        composite_reader::read_composite_closure(&mut source, &first_root, &ReadLimits::default())
            .unwrap_err();
    assert_eq!(memo_error, fresh_error);
}

#[test]
fn the_mixed_chain_reachability_over_real_sessions() {
    // The reachability model over the REAL session roots: a record
    // chain whose founding revision references a LEGACY /1 snapshot
    // object and whose descendants reference the two composite
    // roots — the verified live-set union covers the legacy object,
    // both roots' chunks, and the shared sealed prefix ONCE.
    let (mut spine, mut director) = scenario_pair(600, &near_region_4());
    let first_legacy = save_session(&spine, &director).unwrap();
    let first = session_chunks::chunk_session(&first_legacy, &ChunkPolicy::default()).unwrap();
    let presence = near_region_4();
    for _ in 0..50 {
        director.apply(&mut spine, std::slice::from_ref(&presence));
        spine.run_tick();
    }
    let second_legacy = save_session(&spine, &director).unwrap();
    let second = session_chunks::chunk_session(&second_legacy, &ChunkPolicy::default()).unwrap();
    let first_ids: std::collections::BTreeSet<Hash> = first
        .section_list()
        .iter()
        .map(|section| section.identity)
        .collect();
    let second_ids: std::collections::BTreeSet<Hash> = second
        .section_list()
        .iter()
        .map(|section| section.identity)
        .collect();
    let distinct_union = first_ids.union(&second_ids).count();

    let (mut source, first_root) = composite_store(&first);
    // The walk reads every referenced object by identity — the
    // composite roots included.
    source.insert(first_root.hash(), first_root.bytes().to_vec());
    let second_root =
        StoredObject::new(session_chunks::try_root_manifest(&second).unwrap()).unwrap();
    source.insert(second_root.hash(), second_root.bytes().to_vec());
    for section in second.section_list() {
        let envelope = composite_manifest::encode_chunk_envelope(section.tag, &section.payload);
        source.insert(envelope.hash(), envelope.into_bytes());
    }
    // The legacy /1 snapshot object (the founding revision's direct
    // content reference).
    let legacy_object = StoredObject::new(first_legacy.clone()).unwrap();
    source.insert(legacy_object.hash(), legacy_object.bytes().to_vec());

    let founding = RevisionRecord::founding(AuthorityMode::Standalone, "k2b-mixed", 0)
        .with_content_refs(vec![legacy_object.hash()]);
    let middle =
        RevisionRecord::regular(founding.id(), 0, 1).with_content_refs(vec![first_root.hash()]);
    let head =
        RevisionRecord::regular(middle.id(), 1, 2).with_content_refs(vec![second_root.hash()]);
    source.insert(founding.id(), founding.canonical_bytes());
    source.insert(middle.id(), middle.canonical_bytes());
    source.insert(head.id(), head.canonical_bytes());

    let mut memo = VerificationMemo::new();
    let live = dark_reachability::live_set(
        &mut source,
        &[head.id()],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap_or_else(|error| panic!("the real mixed chain walks: {error:?}"));
    assert_eq!(live.visited_revisions.len(), 3);
    assert_eq!(
        live.composite_roots,
        [first_root.hash(), second_root.hash()]
            .into_iter()
            .collect()
    );
    assert_eq!(
        live.direct_objects,
        [legacy_object.hash()].into_iter().collect()
    );
    assert!(
        live.chunk_identities
            .contains(&first.ledger_chunks[0].identity)
    );
    assert_eq!(
        live.chunk_identities.len(),
        distinct_union,
        "the shared sealed prefix counts once in the verified live-set union"
    );
}

// ------------------------------------------------------- the microbenchmarks

/// The S6-C4 Phase 2 K2-B pod-only performance model (run
/// explicitly: `cargo test -p engine-slice --test k2b_reader --
/// --ignored the_k2b_microbenchmarks --nocapture`). NOTHING here
/// claims production performance improvements — the numbers
/// attribute the CURRENT dark read-side costs (the closure read
/// and hash verification, the memo's effect over shared chunks,
/// the reconstruction, the strict restoration, the memory surface,
/// and the reachability traversal) on the same representative
/// states and runner limits as the K1 microbenchmarks.
#[test]
#[ignore = "the pod-only K2-B microbenchmark — run explicitly with --ignored --nocapture"]
fn the_k2b_microbenchmarks() {
    let samples = 30u32;
    for (label, ticks) in [("mid-scale-600", 600u64), ("large-scale-2400", 2400)] {
        let (mut spine, mut director) = scenario_pair(ticks, &near_region_4());
        let legacy = save_session(&spine, &director).unwrap();
        let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
        let (mut source, root) = composite_store(&chunks);
        // The reachability walk reads the roots by identity — the
        // root objects join the store.
        source.insert(root.hash(), root.bytes().to_vec());
        let entry_count = chunks.section_list().len();
        println!(
            "K2B-BENCH[{label}] workload: ticks={ticks} entries={entry_count} legacy-bytes={} composed={}",
            legacy.len(),
            chunks
                .section_list()
                .iter()
                .map(|s| s.payload.len())
                .sum::<usize>()
        );

        // (1) The full closure read + hash verification (no memo).
        let verified = timed_median_worst(samples, "closure-read", || {
            source.reads = 0;
            let verified = composite_reader::read_composite_closure(
                &mut source,
                &root,
                &ReadLimits::default(),
            )
            .unwrap();
            assert_eq!(source.reads, entry_count);
            verified
        });
        println!(
            "K2B-BENCH[{label}] closure-read: median {:.3} ms · worst {:.3} ms · {} chunks verified",
            verified.0, verified.1, entry_count
        );

        // (2) The reconstruction + the strict restoration.
        let reconstructed = timed_median_worst(samples, "legacy-reconstruction", || {
            session_chunks::reconstruct_legacy_bytes(&verified.2).unwrap()
        });
        assert_eq!(reconstructed.2, legacy);
        println!(
            "K2B-BENCH[{label}] legacy-reconstruction: median {:.3} ms · worst {:.3} ms · {} bytes",
            reconstructed.0,
            reconstructed.1,
            reconstructed.2.len()
        );
        let restored = timed_median_worst(samples, "strict-restoration", || {
            let _ = restore_session(&reconstructed.2).unwrap();
        });
        println!(
            "K2B-BENCH[{label}] strict-restoration: median {:.3} ms · worst {:.3} ms",
            restored.0, restored.1
        );

        // (3) The memo's effect over the REAL shared sealed prefix:
        // the advanced successor shares the sealed chunks — with
        // one memo both roots verify the union once; without it,
        // every chunk re-reads.
        let presence = near_region_4();
        for _ in 0..50 {
            director.apply(&mut spine, std::slice::from_ref(&presence));
            spine.run_tick();
        }
        let advanced_legacy = save_session(&spine, &director).unwrap();
        let advanced =
            session_chunks::chunk_session(&advanced_legacy, &ChunkPolicy::default()).unwrap();
        let advanced_root =
            StoredObject::new(session_chunks::try_root_manifest(&advanced).unwrap()).unwrap();
        let first_ids: std::collections::BTreeSet<Hash> = chunks
            .section_list()
            .iter()
            .map(|section| section.identity)
            .collect();
        let advanced_ids: std::collections::BTreeSet<Hash> = advanced
            .section_list()
            .iter()
            .map(|section| section.identity)
            .collect();
        let shared = first_ids.intersection(&advanced_ids).count();
        let distinct_union = first_ids.union(&advanced_ids).count();
        source.insert(advanced_root.hash(), advanced_root.bytes().to_vec());
        for section in advanced.section_list() {
            let envelope = composite_manifest::encode_chunk_envelope(section.tag, &section.payload);
            source.insert(envelope.hash(), envelope.into_bytes());
        }
        let with_memo = timed_median_worst(samples, "two-root-memo-read", || {
            let mut memo = VerificationMemo::new();
            source.reads = 0;
            composite_reader::read_composite_closure_with_memo(
                &mut source,
                &root,
                &ReadLimits::default(),
                &mut memo,
            )
            .unwrap();
            composite_reader::read_composite_closure_with_memo(
                &mut source,
                &advanced_root,
                &ReadLimits::default(),
                &mut memo,
            )
            .unwrap();
            (source.reads, memo.len())
        });
        assert_eq!(
            with_memo.2.0, distinct_union,
            "the memo verifies each distinct chunk exactly once"
        );
        println!(
            "K2B-BENCH[{label}] two-root-memo-read: median {:.3} ms · worst {:.3} ms · {} source reads for {} distinct chunks ({} shared verified once, {} manifest references)",
            with_memo.0,
            with_memo.1,
            with_memo.2.0,
            with_memo.2.1,
            shared,
            entry_count + advanced.section_list().len()
        );
        let without_memo = timed_median_worst(samples, "two-root-no-memo-read", || {
            source.reads = 0;
            composite_reader::read_composite_closure(&mut source, &root, &ReadLimits::default())
                .unwrap();
            composite_reader::read_composite_closure(
                &mut source,
                &advanced_root,
                &ReadLimits::default(),
            )
            .unwrap();
            source.reads
        });
        println!(
            "K2B-BENCH[{label}] two-root-no-memo-read: median {:.3} ms · worst {:.3} ms · {} source reads (every manifest reference re-read)",
            without_memo.0, without_memo.1, without_memo.2
        );

        // (4) The memory surface: the verified payload copies, the
        // composed stream, the reconstructed legacy bytes.
        let payload_copies: usize = verified.2.chunks.iter().map(|c| c.payload.len()).sum();
        println!(
            "K2B-BENCH[{label}] memory-surface: verified-payloads {} · composed {} · reconstructed {} bytes (single-run pod, shared runner)",
            payload_copies,
            verified.2.composed_payloads().len(),
            reconstructed.2.len()
        );

        // (5) The reachability traversal: a synthetic 8-record
        // chain over the two real roots, walked with the shared
        // memo.
        let mut records = Vec::new();
        let mut parent: Option<Hash> = None;
        for epoch in 0..8 {
            let refs = if epoch % 2 == 0 {
                vec![root.hash()]
            } else {
                vec![advanced_root.hash()]
            };
            let record = match parent {
                None => RevisionRecord::founding(AuthorityMode::Standalone, "k2b-bench", 0),
                Some(parent) => RevisionRecord::regular(parent, epoch - 1, epoch),
            }
            .with_content_refs(refs);
            source.insert(record.id(), record.canonical_bytes());
            parent = Some(record.id());
            records.push(record.id());
        }
        let head = *records.last().unwrap();
        let walk = timed_median_worst(samples, "reachability-walk", || {
            let mut memo = VerificationMemo::new();
            source.reads = 0;
            let live = dark_reachability::live_set(
                &mut source,
                &[head],
                &ReachabilityLimits::default(),
                &mut memo,
            )
            .unwrap();
            (live.chunk_identities.len(), source.reads)
        });
        println!(
            "K2B-BENCH[{label}] reachability-walk: median {:.3} ms · worst {:.3} ms · {} live chunks over 8 records with {} source reads",
            walk.0, walk.1, walk.2.0, walk.2.1
        );
    }
}

/// One benchmark family's median/worst over `samples` runs.
fn timed_median_worst<T: Clone>(
    samples: u32,
    label: &str,
    mut f: impl FnMut() -> T,
) -> (f64, f64, T) {
    use std::time::Instant;
    let mut durations = Vec::with_capacity(samples as usize);
    let mut last = None;
    for _ in 0..samples {
        let start = Instant::now();
        let value = f();
        durations.push(start.elapsed().as_secs_f64() * 1000.0);
        last = Some(value);
    }
    durations.sort_by(|a, b| a.total_cmp(b));
    let median = durations[durations.len() / 2];
    let worst = durations[durations.len() - 1];
    let last = last.expect("at least one sample");
    println!("K2B-BENCH-SAMPLE[{label}] all-samples-ms {:?}", durations);
    (median, worst, last)
}
