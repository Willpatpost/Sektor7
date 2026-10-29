//! PP-B3 — the SK/MK checkpoint publication and THE mandatory crash
//! regression (commit 1's acceptance surface, over the REAL crossing
//! machinery): the two-revision composition (SK the compacted
//! snapshot publication through the existing path; MK its immediate
//! child carrying the permanent checkpoint-metadata object —
//! MK.parent = SK exactly, the deterministic SK-derived idempotency
//! key); the metadata's own coherence (the reclaimable range, the
//! skip table with the non-circular self-retirement entry, the
//! membership index carrying both historical identity classes, the
//! digest commitment recomputed over the range's record bytes oldest
//! first, the founding-derived seed); the immediate-second-checkpoint
//! empty-range refusal; and the SK-committed/MK-uncommitted
//! process-termination regression — a REAL child process dies at the
//! genuine in-driver seam after SK commits and before MK: SK stands
//! as an ordinary valid snapshot (the existing safe recovery finds
//! it; no checkpoint status is fabricated, no reclaim eligibility
//! arises), and a retry over the recovered session completes safely
//! with the crashed SK an ordinary indexed publication in the new
//! checkpoint's range.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;

use engine_core::{Decoder, HistoryKind, LodLevel, PresenceSample, Spine, wire::MAX_TAG_LEN};
use engine_persist::{
    CanonicalMedium, CheckpointMetadata, LocalFileMedium, ReclaimedIndexEntry, SkipEntry,
    T1Journal, chain_range_digest, checkpoint_mk_idempotency_key, compute_digest_seed,
};
use engine_slice::crossing::{ARM_AFTER_SK_EXIT, SessionCrossing, TransitionError};
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};

// ------------------------------------------------------------- fixtures

/// A temporary campaign directory (the established suite pattern):
/// a unique child of the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b3-{name}-{}",
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

/// The real pair (the B2 fixture): 150 ticks under the near presence
/// — region 4 live, five far regions dormant since tick 0.
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

/// The durable boundary (the B2 fixture): every region whose target
/// under the presence differs routes through the crossing — the
/// journaled transitions the canonical confirmation later batches.
fn durable_boundary(
    crossing: &mut SessionCrossing,
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    presence: &PresenceSample,
) {
    let now = spine.clock();
    for region in 0..REGION_COUNT {
        let current = director.levels()[region];
        let next = director.target_level(region, std::slice::from_ref(presence));
        if next == current {
            continue;
        }
        if next == LodLevel::Dormant {
            let _journaled = crossing
                .deactivate(spine, director, region, now)
                .expect("the deactivation commits");
        } else if current == LodLevel::Dormant {
            crossing
                .activate(spine, director, region, next, now)
                .expect("the activation installs");
        } else {
            director.transition_region(spine, region, next, now);
        }
    }
}

fn run_ticks_near(
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    presence: &PresenceSample,
    ticks: u64,
) {
    for _ in 0..ticks {
        director.apply(spine, std::slice::from_ref(presence));
        spine.run_tick();
    }
}

/// The first content-domain tag of a canonical object — the domain
/// peek distinguishing an ordinary snapshot publication from a
/// checkpoint-metadata revision.
fn object_domain(bytes: &[u8]) -> String {
    let mut dec = Decoder::new(bytes);
    dec.read_str(MAX_TAG_LEN, "object-domain")
        .unwrap()
        .to_owned()
}

// ----------------------------------------------------- the publication

#[test]
fn the_checkpoint_publication_composes_sk_then_mk_over_the_real_machinery() {
    // THE two-revision composition over the REAL machinery: two
    // ordinary snapshot publications and a canonical confirmation
    // precede the checkpoint; SK publishes through the existing
    // path; MK lands as SK's immediate child carrying the metadata
    // object. Every structural rule is asserted against the chain
    // itself: MK.parent = SK exactly; the metadata's known-SK
    // binding; the range (the publications, descent order, the
    // confirmation excluded — an anchor is never reclaimable); the
    // skip table (the non-circular self-retirement entry plus the
    // confirmation's skip past the reclaimable run to the founding);
    // the membership index (both historical identity classes per
    // range revision); the digest recomputed oldest-first over the
    // founding-derived seed; and the deterministic MK key.
    let dir = TestDir::new("publication");
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.store(), journal, "publication-campaign").unwrap();
    // The journaled far boundary — the confirmation's evidence (the
    // C1 discipline publishes its covering snapshot before the T1;
    // its revision joins the reclaimable range below).
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let covering = crossing
        .covering_revision()
        .expect("the boundary's covering published");
    run_ticks_near(&mut spine, &mut director, &far, 20);
    let s1 = crossing.publish_snapshot(&spine, &director).unwrap();
    run_ticks_near(&mut spine, &mut director, &far, 20);
    let s2 = crossing.publish_snapshot(&spine, &director).unwrap();
    let engine_persist::CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the journaled captures confirm")
    };
    // C1 — the confirmation revision, the retained pre-checkpoint
    // head (SK's parent-to-be).
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let c1 = medium.head().unwrap().expect("the chain exists");
    drop(medium);

    let report = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the checkpoint publishes");

    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let sk_record = medium.read_record(&report.sk).unwrap();
    let mk_record = medium.read_record(&report.mk).unwrap();
    // THE composition: MK is SK's immediate child, and SK's parent is
    // the retained pre-checkpoint head.
    assert_eq!(
        sk_record.parent,
        Some(c1),
        "SK's parent is the confirmation"
    );
    assert_eq!(
        mk_record.parent,
        Some(report.sk),
        "MK is SK's immediate child"
    );
    // The deterministic MK key (the SK-derived retry identity).
    assert_eq!(
        mk_record.idempotency_key,
        Some(checkpoint_mk_idempotency_key(&report.sk)),
        "MK carries the deterministic SK-derived key"
    );
    // THE metadata object: MK's single content reference, decodable,
    // contextually bound to the revision that carries it.
    assert_eq!(
        mk_record.content_refs.len(),
        1,
        "MK references exactly one object"
    );
    let metadata_bytes = medium
        .read_object(mk_record.content_refs.first().unwrap())
        .unwrap();
    let metadata = CheckpointMetadata::decode(&metadata_bytes)
        .expect("MK's object decodes as the checkpoint metadata");
    assert_eq!(metadata.sk, report.sk, "the known-SK binding");
    assert_eq!(
        metadata.sk_parent, c1,
        "the self-retirement target is SK's parent"
    );
    // THE range: the two publications plus the boundary's covering
    // snapshot (the C1 discipline's — a non-confirming session
    // snapshot publication, reclaimable), descent order — the
    // confirmation excluded (an anchor, never reclaimable).
    assert_eq!(
        report.range,
        vec![s2, s1, covering],
        "the reclaimable range, newest first"
    );
    assert_eq!(
        metadata.range_records, 4,
        "the retired-set count (the covering + the two publications + SK's own retirement entry)"
    );
    // THE skip table: the non-circular self-retirement entry (the
    // reserved zero identity naming the referencing MK itself) and
    // C1's skip past the reclaimable run to the founding.
    let summary = medium.validate().unwrap();
    assert_eq!(
        metadata.skips,
        vec![
            SkipEntry::self_skip(c1),
            SkipEntry::new(c1, summary.founding)
        ],
        "the self entry and the confirmation's skip to the founding"
    );
    // THE membership index: both historical identity classes for
    // every range revision — plus SK's own retirement entry (the
    // mandatory condition 2: the retired SK's key stays in the
    // universe) — sorted by hash.
    let mut expected_index = [s1, s2, covering]
        .iter()
        .map(|revision| {
            let record = medium.read_record(revision).unwrap();
            ReclaimedIndexEntry::new(record.idempotency_key.clone(), *revision, record.epoch)
        })
        .collect::<Vec<_>>();
    expected_index.push(ReclaimedIndexEntry::new(
        sk_record.idempotency_key.clone(),
        report.sk,
        sk_record.epoch,
    ));
    expected_index.sort_unstable_by_key(|entry| entry.hash);
    assert_eq!(
        metadata.index, expected_index,
        "the compact membership index"
    );
    // THE digest commitment: recomputed over the range's record
    // canonical bytes OLDEST first, seeded from the founding (the
    // covering's record included — the range's oldest member).
    let seed = compute_digest_seed(&summary.founding);
    assert_eq!(metadata.seed, seed, "the founding-derived seed");
    let covering_bytes = medium.read_record(&covering).unwrap().canonical_bytes();
    let s1_bytes = medium.read_record(&s1).unwrap().canonical_bytes();
    let s2_bytes = medium.read_record(&s2).unwrap().canonical_bytes();
    let expected_digest = chain_range_digest(&seed, &[covering_bytes, s1_bytes, s2_bytes]);
    assert_eq!(metadata.range_digest, expected_digest, "the range digest");
    assert_eq!(
        report.range_digest, expected_digest,
        "the report carries the commitment"
    );
    assert_eq!(
        report.skips, 2,
        "the skip-table size (the self entry included)"
    );
    // The whole chain — MK included — validates end to end, and MK
    // is the head.
    assert_eq!(summary.head, report.mk, "MK is the head");
}

#[test]
fn an_immediate_second_checkpoint_refuses_the_empty_range() {
    // The refusal rule: a checkpoint whose range is EMPTY (the prior
    // MK is SK's immediate parent — nothing reclaimable lies between)
    // never publishes a pointless MK. The refusal is honest and
    // fail-safe: the freshly published SK stands as an ordinary
    // snapshot publication (never checkpoint metadata), the chain
    // validates, and the NEXT checkpoint after new work would index
    // it normally.
    let dir = TestDir::new("empty-range");
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.store(), journal, "empty-range-campaign").unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let covering = crossing
        .covering_revision()
        .expect("the boundary's covering published");
    run_ticks_near(&mut spine, &mut director, &far, 20);
    let s1 = crossing.publish_snapshot(&spine, &director).unwrap();
    let first = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the first checkpoint publishes");
    assert_eq!(
        first.range,
        vec![s1, covering],
        "the first range carries the publication plus the boundary's covering"
    );

    // New work so the second attempt's COMPACTION succeeds — the
    // refusal under test is the RANGE's emptiness, not the window's.
    run_ticks_near(&mut spine, &mut director, &far, 20);
    let detail = match crossing.publish_checkpoint(&mut spine, &mut director, 10) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the immediate second checkpoint refuses, got {other:?}"),
        Ok(_) => panic!("the immediate second checkpoint must refuse"),
    };
    assert_eq!(detail, "checkpoint-empty-range");

    // The refusal's durable footprint: the head is the second SK —
    // an ordinary snapshot publication, NEVER checkpoint metadata —
    // and the whole chain validates.
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let head = medium.head().unwrap().expect("the chain exists");
    let head_record = medium.read_record(&head).unwrap();
    let head_object = medium
        .read_object(head_record.content_refs.first().unwrap())
        .unwrap();
    assert_eq!(
        head_record.parent,
        Some(first.mk),
        "the refused SK's parent is the first MK"
    );
    assert!(
        object_domain(&head_object).starts_with("engine-slice/session-snapshot"),
        "the head is an ordinary snapshot publication — no MK landed"
    );
    medium.validate().unwrap();
}

// ------------------------------------------------- the crash regression

const CHILD_TEST: &str = "pp_b3_child_crash_after_sk";
const CHILD_STORE_ENV: &str = "S7_PP_B3_CHILD_STORE";
const CHILD_JOURNAL_ENV: &str = "S7_PP_B3_CHILD_JOURNAL";

/// The child-process entry: in a normal `cargo test` run the store
/// variable is absent and this is a no-op — the parent spawns this
/// same binary with `--exact pp_b3_child_crash_after_sk` and the
/// variables set. THE MANDATORY PP-B3 REGRESSION: the child arms the
/// genuine in-driver seam and dies — real process termination —
/// AFTER SK commits and BEFORE MK.
#[test]
fn pp_b3_child_crash_after_sk() {
    let Ok(store) = std::env::var(CHILD_STORE_ENV) else {
        return;
    };
    let store = PathBuf::from(store);
    let journal_path =
        PathBuf::from(std::env::var(CHILD_JOURNAL_ENV).expect("the child journal env"));
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(journal_path).unwrap();
    let mut crossing = SessionCrossing::open(&store, journal, "pp-b3-crash").unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 20);
    // A prior publication — the checkpoint's range will be
    // non-empty, so the death lands at the genuine seam (after SK,
    // before MK), never at an earlier refusal.
    crossing.publish_snapshot(&spine, &director).unwrap();
    // Arm the seam: the next publish_checkpoint hard-exits the
    // process between SK's commit and MK's.
    ARM_AFTER_SK_EXIT.store(true, Ordering::Relaxed);
    let _ = crossing.publish_checkpoint(&mut spine, &mut director, 10);
    panic!("the armed seam must exit the process before MK");
}

#[test]
fn crash_after_sk_before_mk_leaves_an_ordinary_snapshot_and_the_retry_completes() {
    // THE MANDATORY PP-B3 REGRESSION — real process termination at
    // the SK-committed/MK-uncommitted boundary: a child process dies
    // at the genuine in-driver seam. The durable aftermath: the head
    // is SK — an ordinary compacted snapshot, never checkpoint
    // metadata; the chain validates; the EXISTING safe recovery
    // reconstructs the session from it (no checkpoint status
    // fabricated, none needed); and a retry over the recovered
    // session completes safely, the crashed SK an ordinary indexed
    // publication in the new checkpoint's range.
    let dir = TestDir::new("crash-after-sk");
    let child = Command::new(std::env::current_exe().expect("current exe"))
        .arg("--exact")
        .arg(CHILD_TEST)
        .env(CHILD_STORE_ENV, dir.store())
        .env(CHILD_JOURNAL_ENV, dir.journal())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("the child spawns");
    assert_eq!(
        child.code(),
        Some(8),
        "the child died at the after-SK seam — SK committed, MK never ran"
    );

    // The durable state: the head is the crashed SK — an ordinary
    // compacted snapshot publication, NEVER checkpoint metadata.
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let crashed_sk = medium.head().unwrap().expect("the chain exists");
    let head_record = medium.read_record(&crashed_sk).unwrap();
    let head_object = medium
        .read_object(head_record.content_refs.first().unwrap())
        .unwrap();
    assert!(
        object_domain(&head_object).starts_with("engine-slice/session-snapshot"),
        "the head is the crashed SK — an ordinary session snapshot"
    );
    // The chain validates end to end — SK is a well-formed
    // publication, and no MK exists.
    let summary = medium.validate().unwrap();
    assert_eq!(summary.head, crashed_sk);
    drop(medium);

    // THE EXISTING SAFE RECOVERY PATH: SK is an ordinary snapshot —
    // the recovery reconstructs the session from it exactly as it
    // would any compacted publication.
    let (mut spine, mut director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "pp-b3-crash")
            .expect("the crashed SK recovers through the ordinary path");
    assert!(
        matches!(spine.history_kind(), HistoryKind::Compacted { .. }),
        "SK was the compacted snapshot — the save/2 form round-tripped"
    );

    // THE RETRY: new work over the recovered session, then a fresh
    // checkpoint completes safely — the crashed SK is an ordinary
    // indexed publication in the NEW range (reclaimable when
    // superseded), never a falsely completed checkpoint.
    run_ticks_near(&mut spine, &mut director, &near_region_4(), 20);
    let report = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the retry checkpoint completes over the recovered session");
    assert!(
        report.range.contains(&crashed_sk),
        "the crashed SK is an ordinary indexed publication in the new range"
    );
    // The retry's MK decodes and stands as the validated head.
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    let summary = medium.validate().unwrap();
    assert_eq!(summary.head, report.mk, "the retry MK is the head");
    let mk_record = medium.read_record(&report.mk).unwrap();
    let metadata_bytes = medium
        .read_object(mk_record.content_refs.first().unwrap())
        .unwrap();
    let metadata = CheckpointMetadata::decode(&metadata_bytes)
        .expect("the retry MK's object decodes as the metadata");
    assert_eq!(metadata.sk, report.sk, "the retry's known-SK binding");
}
