//! The S6-C4 Phase 2 K1 differential corpus — the dark structural
//! snapshot codec's equivalence and boundary study. K1 is the
//! non-publishing foundation: no canonical storage, no production
//! path — the differential proves the section-split representation
//! composes back to the EXACT accepted legacy serialization, restores
//! through the unchanged strict contract, continues identically,
//! and holds the ledger chunk invariants (the sealed-prefix
//! stability, the boundary cases, the compaction invalidation).
//!
//! The corpus: freshly initialized, early, middle, and large
//! simulation clocks (the multi-chunk ledger at the ratified
//! boundary and the study boundaries); scheduled, blocked, held,
//! and dependent obligations; nonempty consequence ledgers and
//! multiple LOD episodes (the crossing fixtures' geometry); the
//! full-history and the compacted-history spine forms; empty
//! sections and exact chunk boundaries; malformed and truncated
//! chunks.

use std::path::PathBuf;

use engine_core::{PresenceSample, Spine};
use engine_persist::T1Journal;
use engine_slice::crossing::{SessionCrossing, save_session};
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector, stage4_state_fingerprint};
use engine_slice::session_chunks::{self, ChunkPolicy, HistoryForm};

// ------------------------------------------------------------- fixtures

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-k1-chunks-{name}-{}",
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

fn far_from_region_4() -> PresenceSample {
    PresenceSample {
        position: (100.0, 0.0),
        velocity: (0.0, 0.0),
    }
}

/// The real scenario pair (the crossing suite's fixture): the
/// Lod-mode spine plus the director, driven through the driver's own
/// per-tick sequence.
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

/// The full differential over one state (consumed by value — the
/// continuation needs both the original and the reconstructed
/// mutable): the legacy serialization, the chunk round trip, the
/// byte-for-byte composition equality, the unchanged strict
/// restoration, the legacy re-serialization of the restored state,
/// the structural fingerprints, and the identical deterministic
/// continuation of BOTH states. The compacted-history form compares
/// on the compact-safe domain (the full-history fingerprints are
/// contract violations on a compacted spine — the compaction
/// suite's discipline).
fn differential(mut spine: Spine<NpcWorld>, mut director: RegionDirector, policy: &ChunkPolicy) {
    let full_history = spine.history_kind() == engine_core::HistoryKind::Full;
    let fingerprint_before = full_history.then(|| stage4_state_fingerprint(&spine, &director));
    let compact_before = (
        spine.clock(),
        spine.stats(),
        spine.held_count(),
        spine.held_fingerprint(),
        spine.world().milestones().to_vec(),
        director.levels(),
    );
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, policy)
        .unwrap_or_else(|error| panic!("the walk succeeds: {error:?}"));

    // (2→3→5) The composition is byte-for-byte the legacy bytes.
    let composed = session_chunks::compose_session(&chunks);
    assert_eq!(
        composed, legacy,
        "the chunk composition is byte-for-byte the legacy serialization"
    );

    // (4) The unchanged strict restoration over the composed bytes.
    let (mut restored_spine, mut restored_director) = session_chunks::validate_composed(&chunks)
        .unwrap_or_else(|error| panic!("the composed state restores strictly: {error:?}"));

    // (5) The legacy re-serialization of the restored state is
    // byte-identical (the canonical roundtrip holds through the
    // chunk form).
    let re_saved = save_session(&restored_spine, &restored_director).unwrap();
    assert_eq!(
        re_saved, legacy,
        "the restored state re-serializes identically"
    );

    // (6) The structural state: the fingerprints (full history) or
    // the compact-safe domain (compacted history), and the clock.
    if let Some(before) = fingerprint_before {
        assert_eq!(
            stage4_state_fingerprint(&restored_spine, &restored_director),
            before,
            "the structural fingerprints match"
        );
    }
    assert_eq!(restored_spine.clock(), compact_before.0);
    assert_eq!(restored_spine.stats(), compact_before.1);
    assert_eq!(restored_spine.held_count(), compact_before.2);
    assert_eq!(restored_spine.held_fingerprint(), compact_before.3);
    assert_eq!(restored_spine.world().milestones(), compact_before.4);
    assert_eq!(restored_director.levels(), compact_before.5);
    assert_eq!(
        chunks.walked_clock().unwrap(),
        compact_before.0,
        "the walked header carries the authoritative clock"
    );

    // (7→8) The identical deterministic continuation: BOTH the
    // original and the reconstructed state run the same ticks and
    // must behave identically (the compact-safe domain again for
    // the compacted form).
    let presence = near_region_4();
    for _ in 0..40 {
        director.apply(&mut spine, std::slice::from_ref(&presence));
        spine.run_tick();
        restored_director.apply(&mut restored_spine, std::slice::from_ref(&presence));
        restored_spine.run_tick();
    }
    assert_eq!(spine.stats(), restored_spine.stats());
    assert_eq!(spine.clock(), restored_spine.clock());
    assert_eq!(spine.held_fingerprint(), restored_spine.held_fingerprint());
    assert_eq!(
        spine.world().milestones(),
        restored_spine.world().milestones()
    );
    assert_eq!(director.levels(), restored_director.levels());
    if full_history {
        assert_eq!(
            stage4_state_fingerprint(&spine, &director),
            stage4_state_fingerprint(&restored_spine, &restored_director),
            "the continued futures are identical"
        );
    }
}

// ---------------------------------------------------- the differential corpus

#[test]
fn the_freshly_initialized_state_round_trips() {
    // The empty sections and the zero-record ledger: the corpus's
    // fresh corner.
    let (spine, director) = scenario_pair(0, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    assert_eq!(chunks.form, HistoryForm::Full);
    assert_eq!(
        chunks.ledger_chunks.len(),
        1,
        "the empty ledger carries the uniform empty tail chunk"
    );
    assert_eq!(chunks.ledger_chunks[0].payload.len(), 0);
    assert!(session_chunks::verify_chunk(&chunks.ledger_chunks[0]));
    differential(spine, director, &ChunkPolicy::default());
}

#[test]
fn the_early_middle_and_large_clocks_round_trip() {
    // The corpus's clock sweep: early (a handful of executed
    // records), middle (the standard fixture), large (the
    // multi-chunk ledger at the ratified 4096-record boundary).
    for ticks in [5u64, 150, 600] {
        let (spine, director) = scenario_pair(ticks, &near_region_4());
        differential(spine, director, &ChunkPolicy::default());
    }
}

#[test]
fn the_multi_chunk_ledger_at_the_study_boundaries() {
    // The boundary study: the same large state under several
    // policies — every split must compose back byte-for-byte (no
    // duplicate or missing records by construction) and the chunk
    // counts must follow the policy arithmetic.
    let (spine, director) = scenario_pair(600, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let records = spine.stats().executed;
    assert!(
        records > 4096,
        "the large fixture carries a multi-chunk ledger at the default boundary (executed {records})"
    );
    for boundary in [8u64, 1024, 4096] {
        let policy = ChunkPolicy {
            ledger_records: boundary,
        };
        let chunks = session_chunks::chunk_session(&legacy, &policy).unwrap();
        let composed = session_chunks::compose_session(&chunks);
        assert_eq!(
            composed, legacy,
            "boundary {boundary}: the composition is byte-exact"
        );
        let expected_chunks = records.div_ceil(boundary) as usize;
        assert_eq!(
            chunks.ledger_chunks.len(),
            expected_chunks.max(1),
            "boundary {boundary}: the chunk count arithmetic"
        );
        // Every chunk below the last is exactly the boundary width;
        // the last is the partial tail.
        for chunk in &chunks.ledger_chunks[..expected_chunks.saturating_sub(1)] {
            assert_eq!(
                chunk.payload.len(),
                boundary as usize * 31,
                "boundary {boundary}: the sealed chunk width"
            );
        }
        let tail = chunks.ledger_chunks.last().unwrap();
        let tail_records = tail.payload.len() / 31;
        assert!(
            tail_records > 0 && tail_records <= boundary as usize,
            "boundary {boundary}: the tail is partial (or exactly sealed)"
        );
    }
}

#[test]
fn the_exact_boundary_multiples() {
    // The boundary arithmetic's edge cases: a ledger whose count is
    // an exact multiple of the boundary (the last chunk seals
    // exactly), one more (a fresh single-record tail), and one less.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let records = spine.stats().executed as usize;
    let policy = ChunkPolicy {
        ledger_records: records as u64,
    };
    let chunks = session_chunks::chunk_session(&legacy, &policy).unwrap();
    assert_eq!(
        chunks.ledger_chunks.len(),
        1,
        "the exact-multiple boundary seals the whole ledger as one chunk"
    );
    assert_eq!(
        chunks.ledger_chunks[0].payload.len(),
        records * 31,
        "the sealed whole-ledger chunk"
    );
    let policy = ChunkPolicy {
        ledger_records: records as u64 + 1,
    };
    let chunks = session_chunks::chunk_session(&legacy, &policy).unwrap();
    assert_eq!(chunks.ledger_chunks.len(), 1);
    assert_eq!(chunks.ledger_chunks[0].payload.len(), records * 31);
    let policy = ChunkPolicy {
        ledger_records: (records as u64 - 1).max(1),
    };
    let chunks = session_chunks::chunk_session(&legacy, &policy).unwrap();
    assert_eq!(chunks.ledger_chunks.len(), 2);
    assert_eq!(
        chunks.ledger_chunks[1].payload.len(),
        31,
        "the single-record partial tail"
    );
    let composed = session_chunks::compose_session(&chunks);
    assert_eq!(composed, legacy);
}

#[test]
fn the_lod_transition_and_episode_corpus() {
    // The multi-episode geometry: the far boundary's deactivation
    // (captures + episodes) and the reactivation, over both presence
    // windows — nonempty consequence ledgers and held obligations
    // in the chunked sections.
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    director.apply(&mut spine, &[far_from_region_4()]);
    spine.run_tick();
    differential(spine, director, &ChunkPolicy::default());
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    director.apply(&mut spine, &[far_from_region_4()]);
    spine.run_tick();
    director.apply(&mut spine, &[near_region_4()]);
    spine.run_tick();
    differential(spine, director, &ChunkPolicy::default());
}

#[test]
fn the_compacted_history_form_round_trips() {
    // The /2 spine form: the explicit compaction (the summarized
    // prefix + the retained tail), the compacted-meta section, and
    // the differential over the compacted state.
    let dir = TestDir::new("compacted");
    let (mut spine, mut director) = scenario_pair(600, &near_region_4());
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "k1-compacted").unwrap();
    crossing
        .compact_session(&mut spine, &mut director, 512)
        .expect("the compaction applies");
    drop(crossing);
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    assert_eq!(chunks.form, HistoryForm::Compacted);
    assert!(
        chunks.compacted_meta.is_some(),
        "the compacted-meta section present"
    );
    differential(spine, director, &ChunkPolicy::default());

    // The compaction invalidation: the compacted form's chunk set is
    // unrelated to the pre-compaction full form's (the documented
    // invalidation point — the identities are NOT carried across).
    let (full_spine, full_director) = scenario_pair(600, &near_region_4());
    let full_legacy = save_session(&full_spine, &full_director).unwrap();
    let full_chunks = session_chunks::chunk_session(&full_legacy, &ChunkPolicy::default()).unwrap();
    assert_eq!(full_chunks.form, HistoryForm::Full);
    assert!(full_chunks.compacted_meta.is_none());
    assert_ne!(
        full_chunks.ledger_header.identity, chunks.ledger_header.identity,
        "the compaction rewrote the record stream — the count prefixes differ"
    );
}

// ------------------------------------------------------ the chunk invariants

#[test]
fn the_chunk_identities_are_deterministic_and_prefix_stable() {
    // The deterministic identity within the representation: the same
    // bytes chunk twice → identical identities (the root and every
    // section).
    let (spine, director) = scenario_pair(600, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let first = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let second = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    assert_eq!(
        session_chunks::root_identity(&first),
        session_chunks::root_identity(&second),
        "the root identity is deterministic"
    );
    assert_eq!(first, second, "the whole chunk set is deterministic");

    // The sealed-prefix stability: advance the simulation (the
    // append-only ledger grows; the earlier records are immutable)
    // — the sealed prefix chunk identities are UNCHANGED, the chunk
    // count grows, and the growth lands in the tail.
    let executed_before = spine.stats().executed;
    let mut advanced_spine = spine;
    let mut advanced_director = director;
    for _ in 0..50 {
        advanced_director.apply(&mut advanced_spine, std::slice::from_ref(&near_region_4()));
        advanced_spine.run_tick();
    }
    let advanced_legacy = save_session(&advanced_spine, &advanced_director).unwrap();
    let advanced =
        session_chunks::chunk_session(&advanced_legacy, &ChunkPolicy::default()).unwrap();
    let prefix_before = first.sealed_prefix_identities();
    let prefix_after = advanced.sealed_prefix_identities();
    assert!(
        prefix_after.len() >= prefix_before.len(),
        "the sealed prefix only grows under the append-only ledger"
    );
    assert_eq!(
        &prefix_after[..prefix_before.len()],
        &prefix_before[..],
        "the sealed prefix identities are stable across saves — the content-addressed reuse"
    );
    assert!(
        advanced_spine.stats().executed > executed_before,
        "the advanced state genuinely executed more records"
    );
    // The header changes (the clock advanced): its identity moves —
    // the small always-changing sections are the expected write
    // surface.
    assert_ne!(
        advanced.header.identity, first.header.identity,
        "the clock-carrying header identity advances"
    );
}

// ------------------------------------------------------------- the negatives

#[test]
fn the_local_chunk_verification_catches_corruption() {
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    for section in chunks.section_list() {
        assert!(
            session_chunks::verify_chunk(section),
            "the intact chunk verifies locally"
        );
    }
    // A corrupted payload fails the local CAS check.
    let mut corrupted = chunks.ledger_chunks[0].clone();
    corrupted.payload[0] ^= 0xff;
    assert!(
        !session_chunks::verify_chunk(&corrupted),
        "the corrupted chunk fails the local content verification"
    );
}

#[test]
fn the_truncated_chunk_fails_the_global_validation_closed() {
    // A truncated ledger chunk composes to a short stream — the
    // EXISTING strict restoration rejects it closed (the global
    // gate; no optimistic cache).
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let mut chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let tail = chunks.ledger_chunks.last_mut().unwrap();
    tail.payload.truncate(tail.payload.len() / 2);
    let outcome = session_chunks::validate_composed(&chunks);
    let error = match outcome {
        Err(error) => error,
        Ok(_) => panic!("the truncated chunk fails the global validation"),
    };
    assert!(
        matches!(
            error,
            engine_core::SaveError::Truncated | engine_core::SaveError::Limit { .. }
        ),
        "the typed fail-closed class: {error:?}"
    );
}

#[test]
fn the_malformed_walk_fails_closed() {
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    // A truncated envelope: the walk's local checks reject.
    let error =
        match session_chunks::chunk_session(&legacy[..legacy.len() / 2], &ChunkPolicy::default()) {
            Err(error) => error,
            Ok(_) => panic!("the truncated envelope fails the walk"),
        };
    assert!(
        matches!(
            error,
            engine_core::SaveError::Truncated
                | engine_core::SaveError::Limit { .. }
                | engine_core::SaveError::Trailing { .. }
        ),
        "the typed walk rejection: {error:?}"
    );
    // A wrong outer domain: rejected.
    let mut wrong = legacy.clone();
    // Corrupt the domain string's first byte (the length-prefixed
    // tag stays well-formed but names a different domain).
    wrong[8] = b'X';
    let error = match session_chunks::chunk_session(&wrong, &ChunkPolicy::default()) {
        Err(error) => error,
        Ok(_) => panic!("the wrong domain fails the walk"),
    };
    assert!(
        matches!(error, engine_core::SaveError::Domain { .. }),
        "the domain rejection: {error:?}"
    );
    // Trailing garbage after the director blob: rejected.
    let mut trailing = legacy.clone();
    trailing.extend_from_slice(b"garbage");
    let error = match session_chunks::chunk_session(&trailing, &ChunkPolicy::default()) {
        Err(error) => error,
        Ok(_) => panic!("the trailing garbage fails the walk"),
    };
    assert!(
        matches!(error, engine_core::SaveError::Trailing { .. }),
        "the trailing rejection: {error:?}"
    );
}

#[test]
fn the_hand_mangled_chunk_fails_the_strict_restoration() {
    // A payload mangled INSIDE a chunk: the local CAS check fails
    // AND the composed stream fails the global gates.
    let (spine, director) = scenario_pair(150, &near_region_4());
    let legacy = save_session(&spine, &director).unwrap();
    let mut chunks = session_chunks::chunk_session(&legacy, &ChunkPolicy::default()).unwrap();
    let tail = chunks.ledger_chunks.last_mut().unwrap();
    let last = tail.payload.len() - 1;
    tail.payload[last] ^= 0xff;
    assert!(!session_chunks::verify_chunk(tail));
    assert!(
        session_chunks::validate_composed(&chunks).is_err(),
        "the mangled record fails the strict restoration closed"
    );
}

// ---------------------------------------------------------- the microbenchmarks

/// The S6-C4 Phase 2 K1 pod-only microbenchmarks (run explicitly:
/// `cargo test -p engine-slice --test k1_chunks -- --ignored
/// the_k1_microbenchmarks --nocapture`). K1 provides no production
/// benefit — these numbers attribute the CURRENT per-phase costs the
/// staged architecture must attack, on identical representative
/// states with repeated samples, medians and worst cases, workload
/// sizes, and the runner's limits stated. NOTHING here claims
/// reduced graphical frame time or canonical storage use.
#[test]
#[ignore = "the pod-only K1 microbenchmark — run explicitly with --ignored --nocapture"]
fn the_k1_microbenchmarks() {
    let samples = 30u32;
    for (label, ticks) in [("mid-scale-600", 600u64), ("large-scale-2400", 2400)] {
        let (spine, director) = scenario_pair(ticks, &near_region_4());
        let executed = spine.stats().executed;
        let ledger_bytes = executed as usize * 31;
        println!(
            "K1-BENCH[{label}] workload: ticks={ticks} executed={executed} ledger-bytes={ledger_bytes}"
        );

        // (1) The legacy complete-session serialization.
        let legacy = timed_median_worst(samples, "legacy-serialization", || {
            save_session(&spine, &director).unwrap()
        });
        println!(
            "K1-BENCH[{label}] legacy-serialization: median {:.3} ms · worst {:.3} ms · bytes {}",
            legacy.0,
            legacy.1,
            legacy.2.len()
        );

        // (2) The chunk representation generation (the walk + the
        // per-chunk envelope construction and hashing).
        let policy = ChunkPolicy::default();
        let chunked = timed_median_worst(samples, "chunk-generation", || {
            session_chunks::chunk_session(&legacy.2, &policy).unwrap()
        });
        let chunk_count = chunked.2.ledger_chunks.len();
        let payload_total: usize = chunked
            .2
            .section_list()
            .iter()
            .map(|section| section.payload.len())
            .sum();
        println!(
            "K1-BENCH[{label}] chunk-generation: median {:.3} ms · worst {:.3} ms · {} ledger chunks · payload-total {} bytes",
            chunked.0, chunked.1, chunk_count, payload_total
        );

        // (3) The full chunked reconstruction (the compose).
        let composed = timed_median_worst(samples, "chunk-reconstruction", || {
            session_chunks::compose_session(&chunked.2)
        });
        assert_eq!(composed.2, legacy.2, "the composition stays byte-exact");
        println!(
            "K1-BENCH[{label}] chunk-reconstruction: median {:.3} ms · worst {:.3} ms · bytes {}",
            composed.0,
            composed.1,
            composed.2.len()
        );

        // (4) The strict global validation (the unchanged full
        // restoration over the composed bytes).
        let validated = timed_median_worst(samples, "strict-global-validation", || {
            session_chunks::validate_composed(&chunked.2).unwrap();
        });
        println!(
            "K1-BENCH[{label}] strict-global-validation: median {:.3} ms · worst {:.3} ms",
            validated.0, validated.1
        );

        // (5) The changed-chunk hashing share: the chunk generation
        // at boundary 1 (one whole-ledger chunk — the minimal
        // per-chunk overhead) against the default boundary (the
        // many-chunk split) isolates the envelope+hash overhead per
        // additional chunk. The immutable-prefix reuse cost TODAY is
        // the same re-hash (the cache is the future checkpoint's
        // work); this measures what the cache would eliminate.
        let one_chunk_policy = ChunkPolicy { ledger_records: 1 };
        let one_chunk = timed_median_worst(samples, "single-chunk-generation", || {
            session_chunks::chunk_session(&legacy.2, &one_chunk_policy).unwrap()
        });
        println!(
            "K1-BENCH[{label}] single-chunk-generation (boundary=1): median {:.3} ms · worst {:.3} ms · {} chunks — the per-chunk envelope+hash overhead vs the default split",
            one_chunk.0,
            one_chunk.1,
            one_chunk.2.ledger_chunks.len()
        );

        // (6) The new-tail serialization: advance the simulation 50
        // ticks and chunk the advanced state — the bytes in chunks
        // whose identities differ from the prior set are the write
        // surface the staged architecture would publish.
        let mut advanced_spine = spine;
        let mut advanced_director = director;
        let presence = near_region_4();
        for _ in 0..50 {
            advanced_director.apply(&mut advanced_spine, std::slice::from_ref(&presence));
            advanced_spine.run_tick();
        }
        let advanced_legacy = save_session(&advanced_spine, &advanced_director).unwrap();
        let advanced = session_chunks::chunk_session(&advanced_legacy, &policy).unwrap();
        let before: std::collections::BTreeSet<_> = chunked
            .2
            .section_list()
            .iter()
            .map(|section| section.identity)
            .collect();
        let changed_bytes: usize = advanced
            .section_list()
            .iter()
            .filter(|section| !before.contains(&section.identity))
            .map(|section| section.payload.len())
            .sum();
        let sealed_prefix_stable =
            advanced.sealed_prefix_identities() == chunked.2.sealed_prefix_identities();
        println!(
            "K1-BENCH[{label}] new-tail-write-surface: {changed_bytes} bytes of {} total ({:.2}%) · sealed-prefix-stable={sealed_prefix_stable} · whole-legacy-bytes {}",
            payload_total,
            100.0 * changed_bytes as f64 / payload_total as f64,
            advanced_legacy.len()
        );

        // (7) The peak temporary memory (the allocation surface, in
        // bytes — the legacy buffer, the chunk payload copies, the
        // envelope copies, the composed buffer): reported as the
        // measured sizes; the runner is a shared CI-scale pod.
        let envelope_total: usize = chunked
            .2
            .section_list()
            .iter()
            .map(|section| section.payload.len() + section.tag.len() + 16)
            .sum();
        println!(
            "K1-BENCH[{label}] memory-surface: legacy {} · payload-copies {} · envelope-copies ~{} · composed {} bytes (single-run pod, shared runner)",
            legacy.2.len(),
            payload_total,
            envelope_total,
            composed.2.len()
        );
    }
}

/// One benchmark family's median/worst over `samples` runs (the
/// returned tuple carries the timings in milliseconds and the last
/// sample's value for the size reporting).
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
    println!("K1-BENCH-SAMPLE[{label}] all-samples-ms {:?}", durations);
    (median, worst, last)
}
