//! PP-B1 §6.2's backup matrix: the quiescent capture (VACUUM INTO
//! over the TRUNCATE checkpoint + the complete journal), the
//! manifest written last binding the set, the isolated-scratch
//! restoration through the REAL B4 recovery (usable only after that
//! verification), the confirmed-but-unmaterialized journal
//! transactions replaying and re-driving exactly-once over the
//! restored pair, and the refusals — incomplete sets, corrupt
//! manifests, tampered journals, truncated databases, mixed-time
//! pairings, and the never-quiet-overwrite rule.

use std::path::PathBuf;
use std::time::Instant;

use engine_core::{Encoder, LodLevel, PresenceSample, Spine};
use engine_persist::{
    AppendOutcome, CanonicalMedium, CommitOutcome, ConsequenceId, Hash, MediumError,
    RevisionRecord, SqliteMedium, StoredObject, T1Journal,
};
use engine_slice::backup::{BACKUP_MANIFEST_DOMAIN, BackupError, backup_campaign, verify_backup};
use engine_slice::crossing::SessionCrossing;
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};
use engine_slice::store::StoreLocation;

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b1-backup-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
    }

    fn db(&self) -> PathBuf {
        self.0.join("store.db")
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.t1")
    }

    /// A fresh backup-set output directory.
    fn output(&self) -> PathBuf {
        self.0.join("backup-set")
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

fn run(
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

fn durable_boundary(
    crossing: &mut SessionCrossing,
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    presence: &PresenceSample,
) {
    let now = spine.clock();
    for region in 0..engine_slice::npc::REGION_COUNT {
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

#[test]
fn quiescent_backup_verifies_and_the_source_stands() {
    let near = near_region_4();
    let far = far_from_region_4();
    // THE CAMPAIGN — the graceful-close arc over the SQLite provider;
    // the close (the final publication + the ratified TRUNCATE
    // checkpoint + the drop) is the quiescent boundary the backup
    // contract requires.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("quiescent");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "backup-campaign",
    )
    .unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the campaign confirms before the close");
    };
    crossing.checkpoint().unwrap();
    drop(crossing);
    let summary_before = SqliteMedium::open_read_only(dir.db())
        .unwrap()
        .validate()
        .unwrap();
    // THE BACKUP — capture, bind, and prove restorable before the
    // set is ever called usable.
    let report = backup_campaign(&dir.db(), &dir.journal(), &dir.output())
        .expect("the quiescent backup captures, binds, and verifies");
    assert_eq!(
        report.summary, summary_before,
        "the captured chain is the source's"
    );
    assert_eq!(report.summary.campaign_id, "backup-campaign");
    assert!(
        dir.output().join("MANIFEST").is_file(),
        "the manifest exists (written last)"
    );
    // The re-verification is idempotent — the set stands.
    let again = verify_backup(&dir.output()).expect("the completed set re-verifies");
    assert_eq!(again, summary_before);
    // The source campaign is intact and still recoverable through
    // the real machinery (never a quiet overwrite — only the ratified
    // checkpoint touched it).
    let (_spine, _director, _crossing) = SessionCrossing::recover_at(
        &StoreLocation::Database(dir.db()),
        &dir.journal(),
        "backup-campaign",
    )
    .expect("the source campaign still recovers after the backup");
    // NEVER a quiet overwrite: the existing output directory is
    // refused.
    assert!(matches!(
        backup_campaign(&dir.db(), &dir.journal(), &dir.output()),
        Err(BackupError::Layout { .. })
    ));
}

#[test]
fn backup_with_unconfirmed_journal_replays_and_redrives_in_scratch() {
    let near = near_region_4();
    let far = far_from_region_4();
    // THE CRASH STATE: the far boundary's transactions are journaled
    // and installed but NOT canonically confirmed — the
    // confirmed-but-unmaterialized class — and the process dies (a
    // quiet disk; the backup's quiescent contract holds).
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("unconfirmed");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "unconfirmed-campaign",
    )
    .unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let pre_crash_milestones = spine.world().milestones().to_vec();
    drop(crossing);
    // THE BACKUP of the unconfirmed state — the verification's own
    // scratch recovery already proved the pair recovers.
    backup_campaign(&dir.db(), &dir.journal(), &dir.output())
        .expect("the unconfirmed state backs up and verifies");
    // THE OPERATOR'S RESTORE into an isolated directory (never the
    // source, never a live campaign): the pair recovered through the
    // real B4 machinery — the journal replay re-installs the
    // deactivation, the captures pending, the milestones exact.
    let restore = dir.0.join("restore");
    std::fs::create_dir(&restore).unwrap();
    std::fs::copy(dir.output().join("store.db"), restore.join("store.db")).unwrap();
    std::fs::copy(dir.output().join("journal.t1"), restore.join("journal.t1")).unwrap();
    let (spine_r, director_r, mut crossing_r) = SessionCrossing::recover_at(
        &StoreLocation::Database(restore.join("store.db")),
        &restore.join("journal.t1"),
        "unconfirmed-campaign",
    )
    .expect("the restored pair recovers");
    assert_eq!(
        director_r.levels()[4],
        LodLevel::Dormant,
        "the plan re-installed"
    );
    assert_eq!(
        director_r.states()[4].episode,
        1,
        "the episode frontier preserved"
    );
    assert!(
        director_r.ledger().pending_total() > 0,
        "the journaled-but-unconfirmed captures pending — nothing stranded"
    );
    assert_eq!(
        spine_r.world().milestones(),
        &pre_crash_milestones[..],
        "the recovery reconstructs the milestones exactly — no double count"
    );
    // THE EXACT-ONCE RE-DRIVE over the restored pair: the canonical
    // confirmation succeeds and captures the region-4 award — the
    // backup pair is not merely recoverable but confirmable.
    let CommitOutcome::Confirmed(receipt) = crossing_r.commit_canonical().unwrap() else {
        panic!("the re-drive over the restored pair confirms");
    };
    assert!(receipt.keys.contains(&ConsequenceId::new(4, 1, 4)));
}

#[test]
fn incomplete_corrupt_and_tampered_sets_are_refused() {
    let near = near_region_4();
    // A small valid campaign and its verified backup.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("refusals");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "refusal-campaign",
    )
    .unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    crossing.checkpoint().unwrap();
    drop(crossing);
    let output = dir.output();
    backup_campaign(&dir.db(), &dir.journal(), &output).unwrap();
    // (a) INCOMPLETE: the captured files without the manifest — the
    // interrupted-copy case (the manifest is written last; its
    // absence means the capture never completed).
    let set = dir.0.join("set-no-manifest");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(output.join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(output.join("journal.t1"), set.join("journal.t1")).unwrap();
    assert!(matches!(
        verify_backup(&set),
        Err(BackupError::Incomplete { .. })
    ));
    // (b) MANIFEST CORRUPT: the truncated manifest fails its wire
    // decode — nothing is guessed.
    let set = dir.0.join("set-truncated-manifest");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(output.join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(output.join("journal.t1"), set.join("journal.t1")).unwrap();
    let mut manifest = std::fs::read(output.join("MANIFEST")).unwrap();
    manifest.truncate(manifest.len() - 5);
    std::fs::write(set.join("MANIFEST"), manifest).unwrap();
    assert!(matches!(
        verify_backup(&set),
        Err(BackupError::ManifestCorrupt { .. })
    ));
    // (c) TAMPERED JOURNAL: a trailing byte appended after the
    // binding — the hash mismatch refuses the set.
    let set = dir.0.join("set-tampered-journal");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(output.join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(output.join("MANIFEST"), set.join("MANIFEST")).unwrap();
    let mut journal_bytes = std::fs::read(output.join("journal.t1")).unwrap();
    journal_bytes.push(0xff);
    std::fs::write(set.join("journal.t1"), journal_bytes).unwrap();
    assert!(matches!(
        verify_backup(&set),
        Err(BackupError::Incoherent { .. })
    ));
    // (d) TRUNCATED DATABASE: a torn capture fails the chain
    // validation — the medium's own fail-closed open.
    let set = dir.0.join("set-truncated-db");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(output.join("journal.t1"), set.join("journal.t1")).unwrap();
    std::fs::copy(output.join("MANIFEST"), set.join("MANIFEST")).unwrap();
    let mut db_bytes = std::fs::read(output.join("store.db")).unwrap();
    db_bytes.truncate(db_bytes.len() - 64);
    std::fs::write(set.join("store.db"), db_bytes).unwrap();
    assert!(verify_backup(&set).is_err(), "the torn database is refused");
}

#[test]
fn mixed_time_pairs_are_refused() {
    let near = near_region_4();
    let far = far_from_region_4();
    // ONE campaign, TWO backups at different chain states.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("mixed");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "mixed-campaign",
    )
    .unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    crossing.checkpoint().unwrap();
    drop(crossing);
    let set_a = dir.0.join("backup-a");
    backup_campaign(&dir.db(), &dir.journal(), &set_a).unwrap();
    // The campaign continues — the chain and the journal both move.
    let (mut spine, mut director, mut crossing) = SessionCrossing::recover_at(
        &StoreLocation::Database(dir.db()),
        &dir.journal(),
        "mixed-campaign",
    )
    .unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    crossing.checkpoint().unwrap();
    drop(crossing);
    let set_b = dir.0.join("backup-b");
    backup_campaign(&dir.db(), &dir.journal(), &set_b).unwrap();
    // Each set verifies on its own.
    assert!(verify_backup(&set_a).is_ok());
    assert!(verify_backup(&set_b).is_ok());
    // MIXED TIME: A's database with B's journal under A's manifest —
    // the journal-hash binding refuses it.
    let mixed = dir.0.join("mixed-journal");
    std::fs::create_dir(&mixed).unwrap();
    std::fs::copy(set_a.join("store.db"), mixed.join("store.db")).unwrap();
    std::fs::copy(set_a.join("MANIFEST"), mixed.join("MANIFEST")).unwrap();
    std::fs::copy(set_b.join("journal.t1"), mixed.join("journal.t1")).unwrap();
    assert!(matches!(
        verify_backup(&mixed),
        Err(BackupError::Incoherent { .. })
    ));
    // MIXED TIME: A's files under B's manifest — the head binding
    // refuses it.
    let mixed = dir.0.join("mixed-manifest");
    std::fs::create_dir(&mixed).unwrap();
    std::fs::copy(set_a.join("store.db"), mixed.join("store.db")).unwrap();
    std::fs::copy(set_a.join("journal.t1"), mixed.join("journal.t1")).unwrap();
    std::fs::copy(set_b.join("MANIFEST"), mixed.join("MANIFEST")).unwrap();
    assert!(matches!(
        verify_backup(&mixed),
        Err(BackupError::Incoherent { .. })
    ));
}

#[test]
fn live_session_holding_the_journal_refuses_the_backup() {
    // THE REVIEW CORRECTIVE (Finding 3), the journal half of the
    // boundary: a live session (a held T1 journal — every crossing
    // holds its sidecar lock for its lifetime) must not silently
    // produce an allegedly coherent backup: the boundary's
    // try-acquisition refuses, and nothing is captured.
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("live-session");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&StoreLocation::Database(dir.db()), journal, "live-campaign")
            .unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    crossing.checkpoint().unwrap();
    drop(crossing);
    // THE LIVE SESSION: an open T1Journal on the campaign journal —
    // exactly what every live crossing holds — takes the sidecar
    // lock for its lifetime.
    let live = T1Journal::open(dir.journal()).unwrap();
    let refusal = backup_campaign(&dir.db(), &dir.journal(), &dir.output()).unwrap_err();
    assert!(matches!(refusal, BackupError::Quiescence { .. }));
    assert!(
        !dir.output().exists(),
        "nothing was captured — the boundary refused before any capture work"
    );
    drop(live);
    // With the session gone, the same backup succeeds.
    backup_campaign(&dir.db(), &dir.journal(), &dir.output())
        .expect("the quiescent backup succeeds after the session closes");
}

#[test]
fn concurrent_canonical_writer_never_yields_a_coherent_backup() {
    // THE CANONICAL-WRITE HALF OF THE BOUNDARY (the review corrective,
    // Finding 3, with the Ubuntu-incident correction of 2026-10-27, run
    // 40580374199): a writer outside the journal discipline (a raw
    // medium append — exactly what the head-stability verification
    // targets) racing the capture. The safety property is that the
    // backup NEVER yields an INCOHERENT usable set: an in-window
    // committed mutation refuses (Quiescence / Contention /
    // Incoherent — each with its own proof); a genuinely quiescent
    // interval may legitimately succeed, and that success is admitted
    // ONLY with independent verification and provenance (the reported
    // head must be a real source state). Writer ACTIVITY alone —
    // proven by the readiness handshake and the counters — is never
    // conflated with an in-window committed mutation.
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("racing-writer");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "racing-campaign",
    )
    .unwrap();
    run(&mut spine, &mut director, &near, 100);
    // Enough distinct publications that the capture window (the
    // checkpoint + VACUUM INTO) is real.
    for _ in 0..4 {
        run(&mut spine, &mut director, &near, 1);
        crossing.publish_snapshot(&spine, &director).unwrap();
    }
    crossing.checkpoint().unwrap();
    drop(crossing);
    // THE CONCURRENT WRITER: a thread appending content-bearing
    // revisions to the source database continuously (no journal —
    // out-of-discipline by construction) while the backup captures.
    let db_path = dir.db();
    let (start_head, start_epoch) = {
        let mut probe = SqliteMedium::open_read_only(&db_path).unwrap();
        let summary = probe.validate().unwrap();
        (summary.head, summary.height)
    };
    // The writer runs until the backup RETURNS (the 10,000 cap only
    // a never-hang guard): the capture window cannot be append-free —
    // the first CI run exposed the fixed-count version's timing
    // dependence (a fast runner finished all appends before the
    // backup's boundary reads, and the backup then correctly captured
    // the post-race quiescent state). The continuous writer makes the
    // race REAL on every machine — but, as the Ubuntu CI failure
    // (run 40580374199) proved, a real race does NOT guarantee a
    // committed mutation INSIDE the protected interval: on a loaded
    // runner the spawned thread may not be scheduled before the
    // backup completes, and a coherent snapshot of the momentarily
    // quiescent source is then the CORRECT outcome. The corrected
    // oracle below therefore distinguishes writer ACTIVITY (proven by
    // the readiness handshake and the attempt counters) from an
    // IN-WINDOW committed mutation (proven only by the specific
    // refusal classes) — and admits a successful backup ONLY when it
    // is independently verified and its chain head is a real source
    // state (the pre-writer head or one of the writer's committed
    // heads, recorded in the shared vector).
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = stop.clone();
    // The deterministic writer-activity evidence (the CI corrective):
    // every append attempt and every ACCEPTED append are counted —
    // the race is proven firsthand, never assumed from thread
    // scheduling. The READINESS handshake makes the aliveness proof
    // independent of host scheduling: the main thread waits (bounded)
    // for the writer's first attempt BEFORE starting the backup, so
    // `attempts > 0` no longer depends on whether the spawned thread
    // was scheduled — while proving nothing about WHERE any commit
    // landed (that remains the refusal classes' job).
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let writer_attempts = attempts.clone();
    let writer_accepted = accepted.clone();
    // The writer's committed heads, recorded for the success-path
    // provenance check (a synchronized, inspectable structure).
    let committed_heads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let writer_heads = committed_heads.clone();
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_ready = ready.clone();
    let writer = std::thread::spawn(move || {
        let mut medium = SqliteMedium::open(db_path).unwrap();
        let mut head = start_head;
        let mut epoch = start_epoch;
        let mut index = 0u64;
        while !writer_stop.load(std::sync::atomic::Ordering::Relaxed) && index < 10_000 {
            writer_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            writer_ready.store(true, std::sync::atomic::Ordering::SeqCst);
            let mut enc = Encoder::new();
            enc.write_str("sektor7/engine-slice/backup-race/1");
            enc.write_bytes(&index.to_le_bytes());
            let object = StoredObject::new(enc.into_bytes()).unwrap();
            let record = RevisionRecord::regular(head, epoch, 10_000 + index)
                .with_idempotency_key(format!("race-{index}").into_bytes())
                .with_content_refs(vec![object.hash()]);
            match medium.append(&record, std::slice::from_ref(&object), Some(head)) {
                Ok(AppendOutcome::Accepted { id, .. }) => {
                    writer_accepted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    writer_heads.lock().unwrap().push(id);
                    head = id;
                    epoch += 1;
                }
                Ok(other) => panic!("the race append must be accepted, got {other:?}"),
                // A lock-level contention with the backup's own write
                // operations (the checkpoint): the append did not
                // happen — skip this record and keep writing (the
                // parent and its epoch are unchanged).
                Err(MediumError::Contention { .. }) => {}
                Err(other) => panic!("the race append failed: {other}"),
            }
            index += 1;
        }
    });
    // The bounded readiness handshake: the writer has made at least
    // one append attempt before the backup starts (aliveness —
    // never an in-window-mutation claim).
    let handshake = Instant::now() + std::time::Duration::from_secs(10);
    while !ready.load(std::sync::atomic::Ordering::SeqCst) {
        if Instant::now() >= handshake {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            writer.join().unwrap();
            panic!("the racing writer never became ready");
        }
        std::hint::spin_loop();
    }
    let result = backup_campaign(&dir.db(), &dir.journal(), &dir.output());
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
    // The writer demonstrably ran (the handshake made this
    // deterministic) — aliveness, never an in-window-mutation proof.
    assert!(
        attempts.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "the concurrent writer demonstrably ran"
    );
    // THE CORRECTED OUTCOME ORACLE: the safety property is that the
    // backup NEVER yields an incoherent usable set. A refusal is the
    // boundary at work; a success is admissible ONLY as an
    // independently verified coherent snapshot of a REAL source
    // state — never merely because the function returned Ok, and
    // never with the fabricated claim that the writer did not run.
    match result {
        Err(refusal) => {
            // THE REFUSAL CLASSES — three legitimate fail-closed
            // outcomes of the one boundary. Quiescence: the canonical
            // head moved across the stability reads (a committed
            // mutation INSIDE the capture). Contention: the capture's
            // TRUNCATE checkpoint contended the writer's SQLite write
            // lock. Incoherent (the macOS CI evidence, run
            // 40445534468): the writer committed BETWEEN the source
            // validation and the first head read — the stability pair
            // then straddles a quiet interval while the captured chain
            // differs from the pre-capture validated summary, and the
            // audit refuses the set BEFORE any manifest. The
            // Incoherent admission is constrained to exactly the
            // proven race condition and carries its committed-mutation
            // proof; every other outcome remains a failure.
            match &refusal {
                BackupError::Quiescence { .. }
                | BackupError::Medium(MediumError::Contention { .. }) => {}
                BackupError::Incoherent { detail } => {
                    assert!(
                        detail.contains(
                            "the captured database does not validate to the source chain"
                        ),
                        "only the proven race condition's Incoherent is admitted: {detail}"
                    );
                    assert!(
                        accepted.load(std::sync::atomic::Ordering::Relaxed) > 0,
                        "the Incoherent refusal requires a committed concurrent revision — \
                         the captured chain differs from the validated source chain"
                    );
                }
                other => panic!("the refusal is the boundary at work: {other:?}"),
            }
            // A refused backup never publishes a completion manifest
            // (the manifest is written LAST — every refusal above
            // precedes it) and is never usable: the verification path
            // refuses the set.
            assert!(
                !dir.output().join("MANIFEST").is_file(),
                "the refused backup never publishes a completion manifest"
            );
            let verification = verify_backup(&dir.output());
            assert!(
                matches!(verification, Err(BackupError::Incomplete { .. })),
                "the refused set is refused by the verification path (the manifest is \
                 absent — the capture never completed): {verification:?}"
            );
        }
        Ok(report) => {
            // THE SUCCESS PATH — a coherent snapshot of a genuinely
            // quiescent interval: admissible ONLY with independent
            // verification AND provenance. The reported head must be
            // a REAL source state — the pre-writer head (the writer's
            // commits all landed outside the protected interval) or
            // one of the writer's committed heads (recorded by the
            // writer thread) — and the set must pass the real
            // verification (the manifest, the re-validation, the
            // journal rescan, and the isolated-scratch B4 recovery).
            let verification = verify_backup(&dir.output());
            assert!(
                verification.is_ok(),
                "a successful backup must verify: {verification:?}"
            );
            let known_heads = committed_heads.lock().unwrap();
            let head_is_real =
                report.summary.head == start_head || known_heads.contains(&report.summary.head);
            assert!(
                head_is_real,
                "the reported head {} is a real source state (the pre-writer head \
                 {} or one of the {} committed writer heads)",
                report.summary.head,
                start_head,
                known_heads.len()
            );
            // Never the fabricated converse claim: the writer DID run
            // (the handshake and the counters prove activity); the
            // success proves only that no committed mutation landed
            // inside the protected interval — the boundary held.
        }
    }
}

/// The DETERMINISTIC QUIESCENT-SUCCESS regression (the Ubuntu CI
/// incident's lesson, run 40580374199): an independent writer that
/// CANNOT commit until after the backup completes must never cause a
/// refusal — the backup succeeds, verifies, and captures the known
/// quiescent source exactly; a LATER writer commit does not
/// retroactively invalidate the captured snapshot. The writer waits
/// on a bounded go/stop handshake (a channel receive with a timeout
/// plus a hard wall-clock cap) — it can never remain permanently
/// blocked on a test failure.
#[test]
fn quiescent_source_backup_succeeds_and_later_writes_do_not_invalidate() {
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    let dir = TestDir::new("quiescent-success");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "quiescent-campaign",
    )
    .unwrap();
    run(&mut spine, &mut director, &near_region_4(), 100);
    for _ in 0..4 {
        run(&mut spine, &mut director, &near_region_4(), 1);
        crossing.publish_snapshot(&spine, &director).unwrap();
    }
    crossing.checkpoint().unwrap();
    drop(crossing);
    // The known quiescent source head, read before the writer exists.
    let quiescent_head = {
        let mut probe = SqliteMedium::open_read_only(dir.db()).unwrap();
        probe.validate().unwrap().head
    };
    // The gated writer: it cannot commit until the test releases the
    // go signal AFTER the backup completes (or exits on the stop
    // signal / the 60-second safety cap — never permanently blocked).
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = stop.clone();
    let committed = std::sync::Arc::new(std::sync::Mutex::new(Option::<Hash>::None));
    let writer_committed = committed.clone();
    let writer_db_path = dir.db();
    let writer = std::thread::spawn(move || {
        let started = Instant::now();
        loop {
            if writer_stop.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            if Instant::now() - started > std::time::Duration::from_secs(60) {
                return;
            }
            match go_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(()) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        // Released: one real commit onto the source.
        let mut medium = SqliteMedium::open(&writer_db_path).unwrap();
        let mut enc = Encoder::new();
        enc.write_str("sektor7/engine-slice/backup-race/1");
        enc.write_bytes(&0u64.to_le_bytes());
        let object = StoredObject::new(enc.into_bytes()).unwrap();
        let (head, epoch) = {
            let mut probe = SqliteMedium::open_read_only(&writer_db_path).unwrap();
            let summary = probe.validate().unwrap();
            (summary.head, summary.height)
        };
        let record = RevisionRecord::regular(head, epoch, 20_000)
            .with_idempotency_key(b"quiescent-later-write".to_vec())
            .with_content_refs(vec![object.hash()]);
        match medium.append(&record, std::slice::from_ref(&object), Some(head)) {
            Ok(AppendOutcome::Accepted { id, .. }) => {
                *writer_committed.lock().unwrap() = Some(id);
            }
            Ok(other) => panic!("the later write must be accepted, got {other:?}"),
            Err(other) => panic!("the later write failed: {other}"),
        }
    });
    // The backup over the quiescent source: it MUST succeed.
    let report = backup_campaign(&dir.db(), &dir.journal(), &dir.output()).expect(
        "the quiescent backup succeeds — a gated writer that has not committed \
                 is not a boundary violation",
    );
    // (1) The backup succeeded; (2) the verification succeeds (the
    // manifest, the re-validation, the journal rescan, the
    // isolated-scratch recovery).
    assert!(
        verify_backup(&dir.output()).is_ok(),
        "the quiescent set verifies"
    );
    // (3) The captured head matches the known quiescent source.
    assert_eq!(
        report.summary.head, quiescent_head,
        "the captured head is the known quiescent source head"
    );
    // (4) The preserved source/journal pair is coherent (the
    // verification's scratch recovery already proved the pair; the
    // summary is a validated chain).
    assert_eq!(report.summary.campaign_id, "quiescent-campaign");
    // Release the writer, wait for its commit (bounded by the join —
    // the writer exits within the cap on every path).
    go_tx.send(()).unwrap();
    writer.join().unwrap();
    // (5) A later independent writer commit does not retroactively
    // invalidate the captured snapshot: the set STILL verifies and
    // the captured head is unchanged.
    let later = committed
        .lock()
        .unwrap()
        .expect("the later write committed");
    assert_ne!(later, quiescent_head, "the source head genuinely moved");
    assert!(
        verify_backup(&dir.output()).is_ok(),
        "the later source write does not invalidate the captured snapshot"
    );
    let recheck = {
        let mut probe = SqliteMedium::open_read_only(dir.output().join("store.db")).unwrap();
        probe.validate().unwrap().head
    };
    assert_eq!(
        recheck, quiescent_head,
        "the captured database is untouched by the later source write"
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn hand_crafted_manifests_and_wrong_bindings_are_refused() {
    // THE REVIEW CORRECTIVES (Finding 3 + the final contract
    // completion): a manifest of an older wire version (v1 or the
    // corrective-era v2) is refused by the version check — never
    // silently reinterpreted — and a v3 manifest whose last-committed
    // binding does not match the actual journal is refused by the
    // re-scan.
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("hand-crafted");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "binding-campaign",
    )
    .unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    // A durable boundary — the journal carries committed
    // transactions (a non-trivial last-committed binding).
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    drop(crossing);
    let report = backup_campaign(&dir.db(), &dir.journal(), &dir.output()).unwrap();
    let journal_len = std::fs::metadata(dir.output().join("journal.t1"))
        .unwrap()
        .len();
    // (a) A v1-shaped manifest (no boundary evidence, no journal
    // bindings): the version check refuses it.
    let set = dir.0.join("set-v1-manifest");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(dir.output().join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(dir.output().join("journal.t1"), set.join("journal.t1")).unwrap();
    let mut enc = Encoder::new();
    enc.write_str(BACKUP_MANIFEST_DOMAIN);
    enc.write_u32(1);
    enc.write_bytes(report.summary.head.as_bytes());
    enc.write_bytes(report.journal_sha256.as_bytes());
    std::fs::write(set.join("MANIFEST"), enc.into_bytes()).unwrap();
    assert!(matches!(
        verify_backup(&set),
        Err(BackupError::ManifestCorrupt { .. })
    ));
    // (a2) A v2-SHAPED manifest (the corrective-era field set): the
    // version check refuses it — a v2 manifest is never silently
    // reinterpreted as v3 (the final review's requirement).
    let set = dir.0.join("set-v2-manifest");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(dir.output().join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(dir.output().join("journal.t1"), set.join("journal.t1")).unwrap();
    let mut enc = Encoder::new();
    enc.write_str(BACKUP_MANIFEST_DOMAIN);
    enc.write_u32(2);
    enc.write_bytes(report.summary.head.as_bytes());
    enc.write_bytes(report.journal_sha256.as_bytes());
    std::fs::write(set.join("MANIFEST"), enc.into_bytes()).unwrap();
    assert!(matches!(
        verify_backup(&set),
        Err(BackupError::ManifestCorrupt { .. })
    ));
    // (b) A v3 manifest with a WRONG last-committed binding — the
    // journal re-scan refuses it.
    let set = dir.0.join("set-wrong-tx");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(dir.output().join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(dir.output().join("journal.t1"), set.join("journal.t1")).unwrap();
    let wrong_tx = Hash::from_bytes(&[7u8; 32]).unwrap();
    let mut enc = Encoder::new();
    enc.write_str(BACKUP_MANIFEST_DOMAIN);
    enc.write_u32(3);
    enc.write_bytes(report.summary.head.as_bytes());
    enc.write_bytes(report.journal_sha256.as_bytes());
    enc.write_u64(report.summary.records);
    enc.write_str("binding-campaign");
    enc.write_bytes(wrong_tx.as_bytes());
    enc.write_u64(journal_len);
    enc.write_str("/the/source/journal.t1");
    enc.write_u64(1);
    enc.write_u32(1);
    enc.write_u32(1);
    enc.write_u32(1);
    std::fs::write(set.join("MANIFEST"), enc.into_bytes()).unwrap();
    assert!(matches!(
        verify_backup(&set),
        Err(BackupError::Incoherent { .. })
    ));
    // The REAL set still verifies on its own.
    assert!(verify_backup(&dir.output()).is_ok());
}

#[test]
fn manifest_v3_contract_fields_portability_and_bounds() {
    // THE FINAL REVIEW'S CONTRACT COMPLETION: the v3 manifest carries
    // the source-journal identity (PROVENANCE — the journal actually
    // selected and locked; never a restoration requirement: a moved
    // set verifies and restores anywhere) and the capture timestamp
    // (OBSERVATIONAL — never ordering or durability evidence), with
    // presence and bounds enforced at the decode.
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("v3-contract");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&StoreLocation::Database(dir.db()), journal, "v3-campaign")
            .unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    crossing.checkpoint().unwrap();
    drop(crossing);
    let report = backup_campaign(&dir.db(), &dir.journal(), &dir.output()).unwrap();
    let journal_len = std::fs::metadata(dir.output().join("journal.t1"))
        .unwrap()
        .len();
    // The real set's manifest is a v3 (the decode requires the new
    // fields — the round-trip proof) and verifies, including its
    // isolated-scratch restoration.
    assert!(verify_backup(&dir.output()).is_ok());
    // The hand-encoded v3 helper: the correct bindings plus the
    // caller-chosen source-journal identity and capture timestamp.
    let encode_v3 = |source_journal: &str, captured_at: u64| {
        let last_tx = T1Journal::scan_file(dir.output().join("journal.t1"))
            .unwrap()
            .committed
            .last()
            .map(|transaction| transaction.id.0)
            .unwrap_or_else(|| Hash::from_bytes(&[0u8; 32]).unwrap());
        let mut enc = Encoder::new();
        enc.write_str(BACKUP_MANIFEST_DOMAIN);
        enc.write_u32(3);
        enc.write_bytes(report.summary.head.as_bytes());
        enc.write_bytes(report.journal_sha256.as_bytes());
        enc.write_u64(report.summary.records);
        enc.write_str("v3-campaign");
        enc.write_bytes(last_tx.as_bytes());
        enc.write_u64(journal_len);
        enc.write_str(source_journal);
        enc.write_u64(captured_at);
        enc.write_u32(1);
        enc.write_u32(1);
        enc.write_u32(1);
        enc.into_bytes()
    };
    // PORTABILITY: a manifest naming a DIFFERENT source journal (the
    // set moved between machines) still verifies — the identity is
    // provenance, never a restoration requirement — including the
    // full scratch restoration inside verify_backup.
    let set = dir.0.join("set-moved");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(dir.output().join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(dir.output().join("journal.t1"), set.join("journal.t1")).unwrap();
    std::fs::write(
        set.join("MANIFEST"),
        encode_v3(
            "/another/machine/campaign/journal.t1",
            1_700_000_000_000_000_000,
        ),
    )
    .unwrap();
    assert!(
        verify_backup(&set).is_ok(),
        "the moved set verifies and restores — the source-journal identity is provenance"
    );
    // PRESENCE: an EMPTY source-journal identity is refused.
    let set = dir.0.join("set-empty-identity");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(dir.output().join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(dir.output().join("journal.t1"), set.join("journal.t1")).unwrap();
    std::fs::write(set.join("MANIFEST"), encode_v3("", 1)).unwrap();
    assert!(matches!(
        verify_backup(&set),
        Err(BackupError::ManifestCorrupt { .. })
    ));
    // BOUNDS: an over-long source-journal identity is refused (the
    // wire bound).
    let set = dir.0.join("set-overlong-identity");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(dir.output().join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(dir.output().join("journal.t1"), set.join("journal.t1")).unwrap();
    let overlong = "j".repeat(5000);
    std::fs::write(set.join("MANIFEST"), encode_v3(&overlong, 1)).unwrap();
    assert!(matches!(
        verify_backup(&set),
        Err(BackupError::ManifestCorrupt { .. })
    ));
}

#[test]
fn concurrent_verifications_of_one_set_all_succeed() {
    // The scratch-collision regression (the recurring macOS CI
    // flake): multiple verifications of the SAME backup set run in
    // parallel — the exact shape that collided two scratch names
    // when a coarse clock handed parallel tests the same nanosecond
    // stamp. Every verification now claims its OWN exclusively
    // created scratch (the pid + stamp name, the exclusive
    // create_dir, the bounded AlreadyExists retry) and succeeds;
    // the cleanup removes only the directory that verification
    // claimed.
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("concurrent-verify");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "concurrent-verify",
    )
    .unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    drop(crossing);
    backup_campaign(&dir.db(), &dir.journal(), &dir.output()).unwrap();

    let output = dir.output();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let output = output.clone();
            std::thread::spawn(move || {
                verify_backup(&output).expect("the concurrent verification succeeds");
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("the verification thread panicked");
    }
}
