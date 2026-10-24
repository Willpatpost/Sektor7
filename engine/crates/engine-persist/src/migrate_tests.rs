//! PP-B1 §6.1's migration matrix: the happy path (copy, parity,
//! seal, activate, archive — the ChainSummary identical end to end),
//! the interruption points (an interrupted copy resumes from the
//! target's head; a post-seal interruption completes the activation
//! and the archive; a completed migration re-runs cleanly), the
//! fail-closed rules (a source that moved between the copy and the
//! cutover is refused at the raw recheck; a foreign target chain is
//! never merged; an unsafe directory layout is refused before
//! anything is touched; a restored older source is never merged),
//! and the seal's admission (new binaries refuse the archived
//! store).

use std::path::{Path, PathBuf};

use engine_core::Encoder;

use crate::local::LocalFileMedium;
use crate::medium::{AppendOutcome, CanonicalMedium, MediumError, StoredObject};
use crate::migrate::{self, MigrationError};
use crate::record::{AuthorityMode, RevisionRecord};
use crate::sqlite::SqliteMedium;

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b1-migrate-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A content object with a distinctive domain-tagged payload (the
/// index makes identities unique per record).
fn object(index: u64) -> StoredObject {
    let mut enc = Encoder::new();
    enc.write_str("sektor7/engine-persist/migration-test/1");
    enc.write_bytes(&index.to_le_bytes());
    StoredObject::new(enc.into_bytes()).unwrap()
}

fn founding_record(campaign: &str) -> RevisionRecord {
    RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0)
        .with_idempotency_key(b"migration-founding".to_vec())
        .with_content_refs(vec![object(0).hash()])
}

fn regular_record(parent: crate::hash::Hash, parent_epoch: u64, index: u64) -> RevisionRecord {
    RevisionRecord::regular(parent, parent_epoch, index)
        .with_idempotency_key(format!("migration-record-{index}").into_bytes())
        .with_content_refs(vec![object(index).hash()])
}

fn append_accepted<M: CanonicalMedium>(
    medium: &mut M,
    record: &RevisionRecord,
    objects: &[StoredObject],
    expected: Option<crate::hash::Hash>,
) -> crate::hash::Hash {
    match medium.append(record, objects, expected).unwrap() {
        AppendOutcome::Accepted { id, .. } => id,
        other => panic!("the fixture append must be accepted, got {other:?}"),
    }
}

/// A legacy store with a founding plus `regulars` regular records,
/// each referencing its own content object.
fn legacy_store(root: &Path, campaign: &str, regulars: u64) -> LocalFileMedium {
    let mut medium = LocalFileMedium::open(root).unwrap();
    let founding = founding_record(campaign);
    let mut head = append_accepted(&mut medium, &founding, &[object(0)], None);
    let mut epoch = 0u64;
    for index in 1..=regulars {
        let record = regular_record(head, epoch, index);
        head = append_accepted(&mut medium, &record, &[object(index)], Some(head));
        epoch += 1;
    }
    medium
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let to_child = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to_child);
        } else {
            std::fs::copy(entry.path(), &to_child).unwrap();
        }
    }
}

#[test]
fn happy_path_migrates_seals_and_archives() {
    let dir = TestDir::new("happy");
    let legacy_root = dir.0.join("store");
    let target_db = dir.0.join("store.db");
    let journal = dir.0.join("journal.t1");
    let mut legacy = legacy_store(&legacy_root, "happy-campaign", 3);
    let expected = legacy.validate().unwrap();
    drop(legacy);
    let report = migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap();
    assert_eq!(report.copied, 4, "the founding plus three regulars");
    assert_eq!(report.target_summary, expected, "the ChainSummary parity");
    assert_eq!(
        report.source_summary.as_ref().unwrap(),
        &expected,
        "the report carries the validated source summary"
    );
    assert!(!report.resumed);
    let archive = report.archived_to.as_ref().unwrap();
    assert!(archive.is_dir());
    assert!(!legacy_root.exists(), "the legacy root moved out of the way");
    assert!(archive.join("MIGRATED").is_file(), "the seal traveled with it");
    // The seal's admission: a new binary refuses the archived store.
    assert!(matches!(
        LocalFileMedium::open(archive),
        Err(MediumError::Sealed { .. })
    ));
    // The target validates and the campaign identity carried over.
    let mut target = SqliteMedium::open(&target_db).unwrap();
    assert_eq!(target.validate().unwrap(), expected);
    assert_eq!(
        target.campaign().map(|(_, _, id)| id.to_owned()),
        Some("happy-campaign".to_owned())
    );
    // The checklist is the operator's own verification list — the
    // tool never claims gameplay eligibility.
    assert_eq!(report.operator_checklist().len(), 4);
}

#[test]
fn interrupted_copy_resumes_from_the_target_head() {
    let dir = TestDir::new("resume");
    let legacy_root = dir.0.join("store");
    let target_db = dir.0.join("store.db");
    let journal = dir.0.join("journal.t1");
    let mut legacy = legacy_store(&legacy_root, "resume-campaign", 4);
    let expected = legacy.validate().unwrap();
    drop(legacy);
    // THE INTERRUPTED COPY: only the founding plus the first two
    // regulars made it into the target before the process died.
    let mut target = SqliteMedium::open(&target_db).unwrap();
    let founding = founding_record("resume-campaign");
    let mut head = append_accepted(&mut target, &founding, &[object(0)], None);
    let mut epoch = 0u64;
    for index in 1..=2u64 {
        let record = regular_record(head, epoch, index);
        head = append_accepted(&mut target, &record, &[object(index)], Some(head));
        epoch += 1;
    }
    drop(target);
    // THE RESUME: the migration detects the partial target by its
    // head, appends only the missing records, and completes the
    // cutover.
    let report = migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap();
    assert_eq!(report.copied, 2, "only the two un-copied records are appended");
    assert!(!report.resumed);
    assert_eq!(report.target_summary, expected);
    assert!(!legacy_root.exists());
    let mut target = SqliteMedium::open(&target_db).unwrap();
    assert_eq!(target.validate().unwrap(), expected);
}

#[test]
fn completed_migration_reruns_cleanly() {
    let dir = TestDir::new("rerun");
    let legacy_root = dir.0.join("store");
    let target_db = dir.0.join("store.db");
    let journal = dir.0.join("journal.t1");
    let mut legacy = legacy_store(&legacy_root, "rerun-campaign", 2);
    let expected = legacy.validate().unwrap();
    drop(legacy);
    let first = migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap();
    let archive = first.archived_to.clone().unwrap();
    // THE RE-RUN: the source is archived; the report says so and
    // touches nothing.
    let again = migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap();
    assert!(again.resumed);
    assert_eq!(again.copied, 0);
    assert_eq!(again.archived_to, None, "the archive already happened");
    assert_eq!(again.target_summary, expected);
    assert!(archive.is_dir(), "the prior archive is untouched");
    assert!(!legacy_root.exists());
}

#[test]
fn post_seal_interruption_completes_activation_and_archive() {
    let dir = TestDir::new("post-seal");
    let legacy_root = dir.0.join("store");
    let target_db = dir.0.join("store.db");
    let journal = dir.0.join("journal.t1");
    let mut legacy = legacy_store(&legacy_root, "post-seal-campaign", 3);
    let expected = legacy.validate().unwrap();
    let mut target = SqliteMedium::open(&target_db).unwrap();
    let copied = migrate::copy_legacy_chain(&mut legacy, &mut target).unwrap();
    assert_eq!(copied.appended, 4);
    // The migration ran through the full cutover and died before the
    // archive: sealed, activated, still in place.
    migrate::cutover_seal_activate(&legacy_root, &copied.target_summary.head, &mut target)
        .unwrap();
    drop(legacy);
    drop(target);
    assert!(legacy_root.join("MIGRATED").is_file());
    assert!(legacy_root.exists());
    // THE RESUME: the tombstone path — no source read, the activation
    // is idempotent, the archive completes.
    let report = migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap();
    assert!(report.resumed);
    assert_eq!(report.copied, 0);
    assert!(report.archived_to.is_some());
    assert_eq!(report.target_summary, expected);
    assert!(!legacy_root.exists());
    assert!(matches!(
        LocalFileMedium::open(report.archived_to.as_ref().unwrap()),
        Err(MediumError::Sealed { .. })
    ));
}

#[test]
fn seal_without_activation_resumes_and_activates() {
    let dir = TestDir::new("seal-only");
    let legacy_root = dir.0.join("store");
    let target_db = dir.0.join("store.db");
    let journal = dir.0.join("journal.t1");
    let mut legacy = legacy_store(&legacy_root, "seal-only-campaign", 3);
    let expected = legacy.validate().unwrap();
    let mut target = SqliteMedium::open(&target_db).unwrap();
    let copied = migrate::copy_legacy_chain(&mut legacy, &mut target).unwrap();
    drop(legacy);
    drop(target);
    // THE CRASH between the seal and the activation: the tombstone
    // landed (encoded independently here — the wire format pinned),
    // the provenance row did not.
    let mut enc = Encoder::new();
    enc.write_str(migrate::MIGRATED_DOMAIN);
    enc.write_u32(1);
    enc.write_bytes(copied.target_summary.head.as_bytes());
    std::fs::write(legacy_root.join("MIGRATED"), enc.into_bytes()).unwrap();
    // THE RESUME: validate the target against the sealed head,
    // activate over the verified seal, archive.
    let report = migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap();
    assert!(report.resumed);
    assert_eq!(report.copied, 0);
    assert_eq!(report.target_summary, expected);
    assert!(report.archived_to.is_some());
    // The activation landed and is idempotent.
    let mut target = SqliteMedium::open(&target_db).unwrap();
    target.mark_migrated_from(&expected.head).unwrap();
}

#[test]
fn source_moved_between_copy_and_cutover_fails_closed() {
    let dir = TestDir::new("divergence");
    let legacy_root = dir.0.join("store");
    let target_db = dir.0.join("store.db");
    let mut legacy = legacy_store(&legacy_root, "divergence-campaign", 3);
    let expected = legacy.validate().unwrap();
    let mut target = SqliteMedium::open(&target_db).unwrap();
    let copied = migrate::copy_legacy_chain(&mut legacy, &mut target).unwrap();
    drop(legacy);
    // A foreign session appends to the source AFTER the copy: the
    // source head moves (the migration's own validation is stale).
    let mut foreign = LocalFileMedium::open(&legacy_root).unwrap();
    let record = regular_record(expected.head, expected.height, 9);
    append_accepted(&mut foreign, &record, &[object(9)], Some(expected.head));
    drop(foreign);
    // THE CUTOVER: the raw recheck under the held lock refuses.
    let refusal = migrate::cutover_seal_activate(&legacy_root, &copied.target_summary.head, &mut target)
        .unwrap_err();
    assert!(matches!(refusal, MigrationError::Diverged { .. }));
    // Fail-closed: no tombstone (the source is still a live store),
    // the target intact at the copied state.
    assert!(!legacy_root.join("MIGRATED").is_file());
    let mut source = LocalFileMedium::open(&legacy_root).unwrap();
    assert_eq!(source.validate().unwrap().records, expected.records + 1);
    assert_eq!(target.validate().unwrap(), expected);
}

#[test]
fn foreign_target_chain_is_refused() {
    let dir = TestDir::new("foreign-target");
    let legacy_root = dir.0.join("store");
    let target_db = dir.0.join("store.db");
    let journal = dir.0.join("journal.t1");
    let mut legacy = legacy_store(&legacy_root, "campaign-a", 3);
    let expected = legacy.validate().unwrap();
    drop(legacy);
    // The target was founded from a DIFFERENT campaign (a prior,
    // foreign use of the same path).
    let mut target = SqliteMedium::open(&target_db).unwrap();
    let founding_b = founding_record("campaign-b");
    append_accepted(&mut target, &founding_b, &[object(0)], None);
    drop(target);
    // Never merged: the target's head is not on the source chain.
    let refusal =
        migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap_err();
    assert!(matches!(refusal, MigrationError::Diverged { .. }));
    let mut target = SqliteMedium::open(&target_db).unwrap();
    assert_eq!(
        target.validate().unwrap().records,
        1,
        "the foreign target holds only its own founding"
    );
    assert_eq!(target.validate().unwrap().campaign_id, "campaign-b");
    assert!(!legacy_root.join("MIGRATED").is_file(), "the source unsealed");
    let _ = expected;
}

#[test]
fn unsafe_layouts_are_refused_before_anything_is_touched() {
    let dir = TestDir::new("layout");
    let legacy_root = dir.0.join("store");
    drop(legacy_store(&legacy_root, "layout-campaign", 1));
    // The database inside the legacy directory.
    let inside_db = legacy_root.join("nested").join("store.db");
    let journal = dir.0.join("journal.t1");
    assert!(matches!(
        migrate::migrate_local_to_sqlite(&legacy_root, &inside_db, &journal).unwrap_err(),
        MigrationError::Layout { .. }
    ));
    // The journal inside the legacy directory.
    let inside_journal = legacy_root.join("journal.t1");
    assert!(matches!(
        migrate::migrate_local_to_sqlite(&legacy_root, &dir.0.join("store.db"), &inside_journal)
            .unwrap_err(),
        MigrationError::Layout { .. }
    ));
    // The database and the journal are the same path.
    let same = dir.0.join("one-file");
    assert!(matches!(
        migrate::migrate_local_to_sqlite(&legacy_root, &same, &same).unwrap_err(),
        MigrationError::Layout { .. }
    ));
    // Nothing was touched: the source unsealed, no archive, no
    // database, no journal.
    assert!(!legacy_root.join("MIGRATED").is_file());
    assert!(legacy_root.is_dir());
    assert!(!dir.0.join("store.db").exists());
    assert!(!dir.0.join("one-file").exists());
    assert!(find_no_archive(&dir.0, "store"));
}

/// No sibling of `name` in `parent` carries the archive prefix.
fn find_no_archive(parent: &Path, name: &str) -> bool {
    let prefix = format!("{name}.migrated-");
    std::fs::read_dir(parent)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .all(|entry| !entry.file_name().to_string_lossy().starts_with(&prefix))
}

#[test]
fn unfounded_source_is_refused() {
    let dir = TestDir::new("unfounded");
    let legacy_root = dir.0.join("store");
    drop(LocalFileMedium::open(&legacy_root).unwrap());
    let refusal = migrate::migrate_local_to_sqlite(
        &legacy_root,
        &dir.0.join("store.db"),
        &dir.0.join("journal.t1"),
    )
    .unwrap_err();
    assert!(matches!(
        refusal,
        MigrationError::Medium(MediumError::Chain { rule: "unfounded" })
    ));
    assert!(!legacy_root.join("MIGRATED").is_file());
}

#[test]
fn restored_older_source_is_refused_never_merged() {
    let dir = TestDir::new("reappears");
    let legacy_root = dir.0.join("store");
    let backup = dir.0.join("store.backup");
    let target_db = dir.0.join("store.db");
    let journal = dir.0.join("journal.t1");
    let mut legacy = legacy_store(&legacy_root, "reappears-campaign", 3);
    // The operator's pre-migration backup (3 records, no tombstone).
    copy_dir(&legacy_root, &backup);
    let expected = legacy.validate().unwrap();
    drop(legacy);
    // The campaign grows to 5 records, then migrates.
    let mut legacy = LocalFileMedium::open(&legacy_root).unwrap();
    let mut head = expected.head;
    let mut epoch = expected.height;
    for index in 4..=5u64 {
        let record = regular_record(head, epoch, index);
        head = append_accepted(&mut legacy, &record, &[object(index)], Some(head));
        epoch += 1;
    }
    let grown = legacy.validate().unwrap();
    drop(legacy);
    migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap();
    // The operator restores the OLDER backup at the original path.
    std::fs::rename(&backup, &legacy_root).unwrap();
    // The refusal: the database's head (the grown chain) is not on
    // the older chain — divergent histories are never merged.
    let refusal = migrate::migrate_local_to_sqlite(&legacy_root, &target_db, &journal).unwrap_err();
    assert!(matches!(refusal, MigrationError::Diverged { .. }));
    // The target still holds the grown chain; the restored older
    // source is untouched (no second seal, no second archive).
    let mut target = SqliteMedium::open(&target_db).unwrap();
    assert_eq!(target.validate().unwrap(), grown);
    assert!(!legacy_root.join("MIGRATED").is_file());
    assert_eq!(
        LocalFileMedium::open(&legacy_root)
            .unwrap()
            .validate()
            .unwrap()
            .records,
        expected.records,
        "the restored older source stands untouched"
    );
}
