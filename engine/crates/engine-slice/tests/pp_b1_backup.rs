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
    // THE REVIEW CORRECTIVE (Finding 3), the canonical-write half of
    // the boundary: a writer outside the journal discipline (a raw
    // medium append — exactly what the head-stability verification
    // targets) during the capture must refuse the set — never a
    // silent "coherent" backup of a moving campaign.
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
    let writer = std::thread::spawn(move || {
        let mut medium = SqliteMedium::open(db_path).unwrap();
        let mut head = start_head;
        for (epoch, index) in (start_epoch..).zip(0..80u64) {
            let mut enc = Encoder::new();
            enc.write_str("sektor7/engine-slice/backup-race/1");
            enc.write_bytes(&index.to_le_bytes());
            let object = StoredObject::new(enc.into_bytes()).unwrap();
            let record = RevisionRecord::regular(head, epoch, 10_000 + index)
                .with_idempotency_key(format!("race-{index}").into_bytes())
                .with_content_refs(vec![object.hash()]);
            let AppendOutcome::Accepted { id, .. } = medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap()
            else {
                panic!("the race append must be accepted");
            };
            head = id;
        }
    });
    let result = backup_campaign(&dir.db(), &dir.journal(), &dir.output());
    writer.join().unwrap();
    let refusal = result.expect_err("the concurrent writer never yields a usable backup");
    assert!(
        matches!(
            refusal,
            BackupError::Quiescence { .. } | BackupError::Medium(MediumError::Contention { .. })
        ),
        "the refusal is the boundary at work: {refusal:?}"
    );
}

#[test]
fn hand_crafted_manifests_and_wrong_bindings_are_refused() {
    // THE REVIEW CORRECTIVE (Finding 3), the completed §6.2 bindings:
    // a manifest without the v2 fields is refused by the version
    // check, and a v2 manifest whose last-committed binding does not
    // match the actual journal is refused by the re-scan.
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
    // (b) A v2 manifest with a WRONG last-committed binding — the
    // journal re-scan refuses it.
    let set = dir.0.join("set-wrong-tx");
    std::fs::create_dir(&set).unwrap();
    std::fs::copy(dir.output().join("store.db"), set.join("store.db")).unwrap();
    std::fs::copy(dir.output().join("journal.t1"), set.join("journal.t1")).unwrap();
    let wrong_tx = Hash::from_bytes(&[7u8; 32]).unwrap();
    let mut enc = Encoder::new();
    enc.write_str(BACKUP_MANIFEST_DOMAIN);
    enc.write_u32(2);
    enc.write_bytes(report.summary.head.as_bytes());
    enc.write_bytes(report.journal_sha256.as_bytes());
    enc.write_u64(report.summary.records);
    enc.write_str("binding-campaign");
    enc.write_bytes(wrong_tx.as_bytes());
    enc.write_u64(journal_len);
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
