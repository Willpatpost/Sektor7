//! In-crate unit suite for the local file medium: store operations,
//! the insert-once and orphan protocols, the corruption corpus
//! (cold-open fail-closed behavior), the trusted-cursor O(1)
//! property, and the `Unknown`/reconcile semantics. The
//! authorization's real-process obligations — multi-writer races,
//! killed lock holders, the crash-window matrix — live in
//! `tests/process.rs`.
//!
//! A note on cycle coverage: a *hash-verifiable* parent cycle would
//! require a SHA-256 fixed point (an object whose content names its
//! own storage hash), which is infeasible to construct — the cycle
//! guards in the walks are defense in depth behind content
//! addressing. The reachable corruption form (an object claiming a
//! name its bytes do not hash to) is tested and fails closed without
//! hanging.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use engine_core::{Encoder, SaveError};

use crate::fault::{FaultAction, FaultControl, FaultPoint};
use crate::hash::{Hash, hash_object};
use crate::local::{
    HEAD_DOMAIN, HEAD_FILE, HEAD_LOCK, HEAD_VERSION, LocalFileMedium, OBJECTS_DIR, QUARANTINE_DIR,
};
use crate::medium::{
    AppendOutcome, CanonicalMedium, MediumError, Reconciliation, StoredObject, SyncTier,
    UnknownReason,
};
use crate::record::{AuthorityMode, RevisionRecord};

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

/// A unique temp root removed on drop — no `tempfile` dependency; the
/// crate's dependency set is fixed by the authorization.
struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let seq = DIR_SEQ.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("s7-b2-unit-{}-{label}-{seq}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test root");
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn content_object(payload: &[u8]) -> StoredObject {
    let mut enc = Encoder::new();
    enc.write_str("sektor7/test/content/1");
    enc.write_bytes(payload);
    StoredObject::new(enc.into_bytes()).expect("well-formed content object")
}

fn founding(tick: u64) -> RevisionRecord {
    RevisionRecord::founding(AuthorityMode::Standalone, "unit-campaign", tick)
}

fn regular(parent: Hash, parent_epoch: u64, tick: u64) -> RevisionRecord {
    RevisionRecord::regular(parent, parent_epoch, tick)
}

fn object_path(root: &Path, hash: &Hash) -> PathBuf {
    root.join(OBJECTS_DIR).join(hash.hex())
}

fn quarantine_path(root: &Path, hash: &Hash) -> PathBuf {
    root.join(QUARANTINE_DIR).join(hash.hex())
}

/// Installs raw bytes under their content hash (hand-built stores).
fn install_object(root: &Path, bytes: &[u8]) -> Hash {
    let hash = hash_object(bytes);
    let dir = root.join(OBJECTS_DIR);
    fs::create_dir_all(&dir).expect("create objects dir");
    fs::write(dir.join(hash.hex()), bytes).expect("install object");
    hash
}

fn install_head(root: &Path, id: &Hash) {
    let mut enc = Encoder::new();
    enc.write_str(HEAD_DOMAIN);
    enc.write_u32(HEAD_VERSION);
    enc.write_bytes(id.as_bytes());
    fs::write(root.join(HEAD_FILE), enc.into_bytes()).expect("install HEAD");
}

fn append_accepted(
    medium: &mut LocalFileMedium,
    record: &RevisionRecord,
    objects: &[StoredObject],
    expected_parent: Option<Hash>,
) -> Hash {
    let tier = medium.durability_tier();
    match medium
        .append(record, objects, expected_parent)
        .expect("append succeeds")
    {
        AppendOutcome::Accepted { id, tier: observed } => {
            assert_eq!(
                observed, tier,
                "the accepted tier must be the platform tier"
            );
            id
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

fn founded_store(root: &Path) -> (LocalFileMedium, Hash) {
    let mut medium = LocalFileMedium::open(root).expect("open");
    let id = append_accepted(&mut medium, &founding(0), &[], None);
    (medium, id)
}

fn expect_chain(error: MediumError, rule: &'static str) {
    match error {
        MediumError::Chain { rule: observed } => assert_eq!(observed, rule),
        other => panic!("expected chain rule {rule}, got: {other}"),
    }
}

/// `expect_err` for `open` (the medium carries file handles and is
/// intentionally not `Debug`).
fn open_expect_err(root: &Path, why: &str) -> MediumError {
    match LocalFileMedium::open(root) {
        Ok(_medium) => panic!("{why}: open unexpectedly succeeded"),
        Err(error) => error,
    }
}

// ------------------------------------------------------------ foundations

#[test]
fn founding_append_creates_valid_chain() {
    let dir = TestDir::new("founding");
    let mut medium = LocalFileMedium::open(dir.path()).expect("open");
    assert_eq!(medium.campaign(), None, "unfounded store has no campaign");

    let id = append_accepted(&mut medium, &founding(0), &[], None);

    assert_eq!(medium.head().expect("head"), Some(id));
    assert_eq!(
        medium.campaign(),
        Some((id, AuthorityMode::Standalone, "unit-campaign"))
    );
    let record = medium.read_record(&id).expect("read record");
    assert_eq!(record, founding(0));

    let summary = medium.validate().expect("validate");
    assert_eq!(summary.head, id);
    assert_eq!(summary.height, 0);
    assert_eq!(summary.records, 1);
    assert_eq!(summary.authority, AuthorityMode::Standalone);
    assert_eq!(summary.campaign_id, "unit-campaign");
    assert_eq!(summary.content_objects, 0);
}

#[test]
fn regular_appends_advance_and_count() {
    let dir = TestDir::new("advance");
    let (mut medium, f) = founded_store(dir.path());

    let first = content_object(b"payload-one");
    let first_hash = first.hash();
    let first_bytes = first.bytes().to_vec();
    let r1 = regular(f, 0, 1).with_content_refs(vec![first_hash]);
    let r1_id = append_accepted(&mut medium, &r1, std::slice::from_ref(&first), Some(f));

    let second = content_object(b"payload-two");
    let second_hash = second.hash();
    let second_bytes = second.bytes().to_vec();
    // r2 re-references r1's object: the contract requires supplying
    // exactly the declared set — the stored object is hash-verified
    // and reused (insert-once).
    let r2 = regular(r1_id, 1, 2).with_content_refs(vec![second_hash, first_hash]);
    let r2_id = append_accepted(&mut medium, &r2, &[second, first], Some(r1_id));

    assert_eq!(medium.head().expect("head"), Some(r2_id));
    assert_eq!(medium.read_object(&first_hash).expect("read"), first_bytes);
    assert_eq!(
        medium.read_object(&second_hash).expect("read"),
        second_bytes
    );

    let summary = medium.validate().expect("validate");
    assert_eq!(summary.records, 3);
    assert_eq!(summary.height, 2);
    assert_eq!(summary.founding, f);
    // Two distinct content objects across the chain (r2 re-references
    // r1's object).
    assert_eq!(summary.content_objects, 2);
}

#[test]
fn durability_tier_matches_platform() {
    let dir = TestDir::new("tier");
    let medium = LocalFileMedium::open(dir.path()).expect("open");
    #[cfg(unix)]
    assert_eq!(medium.durability_tier(), SyncTier::FullPosixSequence);
    #[cfg(windows)]
    assert_eq!(medium.durability_tier(), SyncTier::ProcessCrashOnly);
}

#[test]
fn head_lock_persists_across_sessions() {
    let dir = TestDir::new("lock-file");
    {
        let (_medium, _f) = founded_store(dir.path());
    }
    let lock = dir.path().join(HEAD_LOCK);
    assert!(lock.exists(), "HEAD.lock is never unlinked");
    {
        let mut medium = LocalFileMedium::open(dir.path()).expect("reopen");
        let summary = medium.validate().expect("validate");
        assert_eq!(summary.records, 1);
    }
    assert!(lock.exists(), "HEAD.lock survives every session");
}

// -------------------------------------------------------------- outcomes

#[test]
fn stale_parent_rejected_with_observed_head() {
    let dir = TestDir::new("stale");
    let (mut medium, f) = founded_store(dir.path());
    let object = content_object(b"payload-r1");
    let r1 = regular(f, 0, 1).with_content_refs(vec![object.hash()]);
    let r1_id = append_accepted(&mut medium, &r1, &[object], Some(f));

    // A late writer still expecting the founding parent.
    let late = regular(f, 0, 2).with_content_refs(vec![content_object(b"late").hash()]);
    match medium
        .append(&late, &[content_object(b"late")], Some(f))
        .expect("append completes")
    {
        AppendOutcome::RejectedStaleParent { observed } => assert_eq!(observed, Some(r1_id)),
        other => panic!("expected stale rejection, got {other:?}"),
    }
    let summary = medium.validate().expect("validate");
    assert_eq!(summary.records, 2, "the stale candidate was not installed");
}

#[test]
fn second_founding_is_stale() {
    let dir = TestDir::new("second-founding");
    let (mut medium, f) = founded_store(dir.path());
    match medium
        .append(&founding(9), &[], None)
        .expect("append completes")
    {
        AppendOutcome::RejectedStaleParent { observed } => assert_eq!(observed, Some(f)),
        other => panic!("expected stale rejection, got {other:?}"),
    }
}

#[test]
fn regular_on_unfounded_is_rejected() {
    let dir = TestDir::new("regular-unfounded");
    let mut medium = LocalFileMedium::open(dir.path()).expect("open");
    let record = regular(Hash::ZERO, 0, 1);
    match medium.append(&record, &[], None).expect("append completes") {
        AppendOutcome::RejectedStaleParent { observed } => assert_eq!(observed, None),
        other => panic!("expected stale rejection, got {other:?}"),
    }
}

// ------------------------------------------------------------ coherence

#[test]
fn founding_epoch_nonzero_rejected() {
    // The (unfounded, founding) coherence arm: the founding record
    // must claim epoch 0 on an empty store.
    let dir = TestDir::new("founding-epoch");
    let mut medium = LocalFileMedium::open(dir.path()).expect("open");
    let mut record = founding(1);
    record.epoch = 5; // fields are pub; the builder cannot express this
    let error = medium
        .append(&record, &[], None)
        .expect_err("incoherent founding");
    expect_chain(error, "kind-incoherent");
}

#[test]
fn founding_with_depends_on_rejected() {
    let dir = TestDir::new("founding-depends");
    let mut medium = LocalFileMedium::open(dir.path()).expect("open");
    let unreachable = hash_object(b"unreachable");
    let record = founding(0).with_depends_on(vec![unreachable]);
    let error = medium
        .append(&record, &[], None)
        .expect_err("founding cannot depend");
    expect_chain(error, "depends-on-unresolvable");
}

#[test]
fn epoch_regression_at_append_rejected() {
    let dir = TestDir::new("epoch-append");
    let (mut medium, f) = founded_store(dir.path());
    // Skips an epoch (2 instead of 1): any deviation from
    // cursor.epoch + 1 is a regression.
    let record = regular(f, 1, 1);
    let error = medium
        .append(&record, &[], Some(f))
        .expect_err("epoch must advance by one");
    expect_chain(error, "epoch-regression");
}

#[test]
fn parent_mismatch_at_append_rejected() {
    let dir = TestDir::new("parent-append");
    let (mut medium, f) = founded_store(dir.path());
    let mut record = regular(f, 0, 1);
    record.parent = Some(hash_object(b"unrelated"));
    let error = medium
        .append(&record, &[], Some(f))
        .expect_err("parent must be the head");
    expect_chain(error, "parent-mismatch");
}

#[test]
fn duplicate_idempotency_key_at_append_rejected() {
    let dir = TestDir::new("dup-key-append");
    let mut medium = LocalFileMedium::open(dir.path()).expect("open");
    let f = founding(0).with_idempotency_key(b"key-one".to_vec());
    let f_id = append_accepted(&mut medium, &f, &[], None);

    let r1 = regular(f_id, 0, 1).with_idempotency_key(b"key-one".to_vec());
    let error = medium
        .append(&r1, &[], Some(f_id))
        .expect_err("keys are chain-unique");
    expect_chain(error, "duplicate-idempotency-key");
}

#[test]
fn depends_on_unresolvable_rejected() {
    let dir = TestDir::new("depends-unreachable");
    let (mut medium, f) = founded_store(dir.path());
    let unreachable = hash_object(b"never-written");
    let record = regular(f, 0, 1).with_depends_on(vec![unreachable]);
    let error = medium
        .append(&record, &[], Some(f))
        .expect_err("dependencies must resolve");
    expect_chain(error, "depends-on-unresolvable");
}

// ------------------------------------------------------------ structural

#[test]
fn noncanonical_record_rejected() {
    let dir = TestDir::new("noncanonical");
    let (mut medium, f) = founded_store(dir.path());
    let low = content_object(b"low");
    let high = content_object(b"high");
    let (low_hash, high_hash) = (low.hash(), high.hash());
    let ordered = if low_hash < high_hash {
        vec![low_hash, high_hash]
    } else {
        vec![high_hash, low_hash]
    };

    // Descending order (bypassing the builder's normalization).
    let mut record = regular(f, 0, 1);
    record.content_refs = vec![ordered[1], ordered[0]];
    let error = medium
        .append(&record, &[low.clone(), high.clone()], Some(f))
        .expect_err("noncanonical order cannot round-trip");
    assert!(
        matches!(error, MediumError::Format(SaveError::Order { .. })),
        "expected a decode order failure, got {error}"
    );

    // Duplicate references.
    let mut record = regular(f, 0, 2);
    record.content_refs = vec![low_hash, low_hash];
    let error = medium
        .append(&record, &[low.clone(), high], Some(f))
        .expect_err("duplicate references cannot round-trip");
    assert!(
        matches!(error, MediumError::Format(SaveError::Order { .. })),
        "expected a decode order failure, got {error}"
    );
}

#[test]
fn content_set_mismatch_rejected() {
    let dir = TestDir::new("content-set");
    let (mut medium, f) = founded_store(dir.path());
    let a = content_object(b"object-a");
    let b = content_object(b"object-b");

    // Declared but not supplied.
    let declared_only = regular(f, 0, 1).with_content_refs(vec![a.hash()]);
    let error = medium
        .append(&declared_only, std::slice::from_ref(&b), Some(f))
        .expect_err("declared references must be supplied");
    expect_chain(error, "content-set-mismatch");

    // Supplied but not declared.
    let supplied_only = regular(f, 0, 2).with_content_refs(vec![a.hash()]);
    let error = medium
        .append(&supplied_only, &[a.clone(), b], Some(f))
        .expect_err("supplied objects must be declared");
    expect_chain(error, "content-set-mismatch");
}

// ------------------------------------------------- unknown and reconcile

#[test]
fn unknown_before_rename_reconciles_not_installed_then_retry_accepts() {
    let dir = TestDir::new("unknown-pre-rename");
    let f = {
        let mut clean = LocalFileMedium::open(dir.path()).expect("open");
        append_accepted(&mut clean, &founding(0), &[], None)
    };

    let object = content_object(b"payload-r");
    let record = regular(f, 0, 1).with_content_refs(vec![object.hash()]);
    let candidate = record.id();

    // The fault fires after HEAD.tmp's sync, before the rename: the
    // append is Unknown, the head is unchanged, and the candidate's
    // objects exist but are unreferenced.
    let mut faulted = LocalFileMedium::open_with(
        dir.path(),
        Some(FaultControl::new().arm(FaultPoint::HeadTmpSynced, FaultAction::InjectError)),
    )
    .expect("open faulted");
    match faulted
        .append(&record, std::slice::from_ref(&object), Some(f))
        .expect("append completes")
    {
        AppendOutcome::Unknown {
            reason: UnknownReason::HeadPublishFailed,
        } => {}
        other => panic!("expected Unknown, got {other:?}"),
    }
    assert_eq!(faulted.head().expect("head"), Some(f));

    // Reconciliation in the same session (no sweep has run): the
    // candidate is present but not installed — quarantined, never
    // deleted, reported retry-safe.
    match faulted.reconcile_unknown(&candidate).expect("reconcile") {
        Reconciliation::NotInstalled { head } => assert_eq!(head, Some(f)),
        other => panic!("expected NotInstalled, got {other:?}"),
    }
    assert!(
        !object_path(dir.path(), &candidate).exists(),
        "the uninstalled candidate was moved out of objects/"
    );
    assert!(
        quarantine_path(dir.path(), &candidate).exists(),
        "the candidate's evidence is preserved in quarantine/"
    );

    // An idempotent retry (same record bytes → the same identity)
    // installs it exactly once.
    let mut retry = LocalFileMedium::open(dir.path()).expect("open retry");
    let retried = append_accepted(&mut retry, &record, &[object], Some(f));
    assert_eq!(retried, candidate);
    let summary = retry.validate().expect("validate");
    assert_eq!(summary.records, 2);
    assert_eq!(summary.head, candidate);
}

#[test]
fn unknown_after_rename_reconciles_installed() {
    let dir = TestDir::new("unknown-post-rename");
    let f = {
        let mut clean = LocalFileMedium::open(dir.path()).expect("open");
        append_accepted(&mut clean, &founding(0), &[], None)
    };

    let object = content_object(b"payload-w");
    let record = regular(f, 0, 1).with_content_refs(vec![object.hash()]);
    let candidate = record.id();

    // The fault fires after the rename: the revision IS installed; the
    // honest outcome is still Unknown (durability of the
    // acknowledgment cannot be claimed).
    let mut faulted = LocalFileMedium::open_with(
        dir.path(),
        Some(FaultControl::new().arm(FaultPoint::AfterHeadRename, FaultAction::InjectError)),
    )
    .expect("open faulted");
    match faulted
        .append(&record, std::slice::from_ref(&object), Some(f))
        .expect("append completes")
    {
        AppendOutcome::Unknown {
            reason: UnknownReason::PostPublishSyncFailed,
        } => {}
        other => panic!("expected Unknown, got {other:?}"),
    }

    match faulted.reconcile_unknown(&candidate).expect("reconcile") {
        Reconciliation::Installed { id } => assert_eq!(id, candidate),
        other => panic!("expected Installed, got {other:?}"),
    }

    // A retry of the same logical operation must not duplicate it.
    let mut retry = LocalFileMedium::open(dir.path()).expect("open retry");
    match retry
        .append(&record, &[object], Some(f))
        .expect("retry completes")
    {
        AppendOutcome::RejectedStaleParent { observed } => assert_eq!(observed, Some(candidate)),
        other => panic!("expected stale rejection on retry, got {other:?}"),
    }
    let summary = retry.validate().expect("validate");
    assert_eq!(summary.records, 2);
    assert_eq!(summary.head, candidate);
}

#[test]
fn unknown_mid_object_write_leaves_head_intact() {
    let dir = TestDir::new("unknown-mid-write");
    let f = {
        let mut clean = LocalFileMedium::open(dir.path()).expect("open");
        append_accepted(&mut clean, &founding(0), &[], None)
    };

    let object = content_object(b"payload-m");
    let record = regular(f, 0, 1).with_content_refs(vec![object.hash()]);
    let candidate = record.id();

    let mut faulted = LocalFileMedium::open_with(
        dir.path(),
        Some(FaultControl::new().arm(FaultPoint::MidObjectWrite, FaultAction::InjectError)),
    )
    .expect("open faulted");
    match faulted
        .append(&record, std::slice::from_ref(&object), Some(f))
        .expect("append completes")
    {
        AppendOutcome::Unknown {
            reason: UnknownReason::ObjectWriteFailed,
        } => {}
        other => panic!("expected Unknown, got {other:?}"),
    }
    // The record object was never written; nothing to quarantine.
    match faulted.reconcile_unknown(&candidate).expect("reconcile") {
        Reconciliation::NotInstalled { head } => assert_eq!(head, Some(f)),
        other => panic!("expected NotInstalled, got {other:?}"),
    }
    assert!(!quarantine_path(dir.path(), &candidate).exists());

    let mut retry = LocalFileMedium::open(dir.path()).expect("open retry");
    let retried = append_accepted(&mut retry, &record, &[object], Some(f));
    assert_eq!(retried, candidate);
    assert_eq!(retry.validate().expect("validate").records, 2);
}

#[test]
fn pre_window_faults_are_clean_errors() {
    for point in [
        FaultPoint::BeforeLock,
        FaultPoint::AfterLock,
        FaultPoint::AfterValidation,
        FaultPoint::AfterComparison,
    ] {
        // A fresh store per point — every case founds its own chain.
        let dir = TestDir::new(point.name());
        let f = {
            let mut clean = LocalFileMedium::open(dir.path()).expect("open");
            append_accepted(&mut clean, &founding(0), &[], None)
        };
        let mut faulted = LocalFileMedium::open_with(
            dir.path(),
            Some(FaultControl::new().arm(point, FaultAction::InjectError)),
        )
        .expect("open faulted");
        let record = regular(f, 0, 1);
        let error = faulted
            .append(&record, &[], Some(f))
            .expect_err("pre-window faults are errors, never Unknown");
        assert!(
            matches!(
                error,
                MediumError::Io {
                    op: "fault-injected",
                    ..
                }
            ),
            "{:?}: expected a clean injected error, got {error}",
            point
        );
        // Nothing was written: the head is unchanged and the store
        // still validates.
        let mut clean = LocalFileMedium::open(dir.path()).expect("reopen");
        assert_eq!(clean.head().expect("head"), Some(f));
        assert_eq!(clean.validate().expect("validate").records, 1);
    }
}

#[test]
fn reconcile_ancestor_is_installed() {
    let dir = TestDir::new("reconcile-ancestor");
    let (mut medium, f) = founded_store(dir.path());
    let r1 = regular(f, 0, 1);
    let r1_id = append_accepted(&mut medium, &r1, &[], Some(f));
    let r2 = regular(r1_id, 1, 2);
    let r2_id = append_accepted(&mut medium, &r2, &[], Some(r1_id));

    // A candidate that is an ancestor of the head is installed — a
    // retry must not duplicate its effect.
    match medium.reconcile_unknown(&r1_id).expect("reconcile") {
        Reconciliation::Installed { id } => assert_eq!(id, r1_id),
        other => panic!("expected Installed, got {other:?}"),
    }
    match medium.reconcile_unknown(&r2_id).expect("reconcile") {
        Reconciliation::Installed { id } => assert_eq!(id, r2_id),
        other => panic!("expected Installed, got {other:?}"),
    }
}

#[test]
fn reconcile_absent_candidate_not_installed() {
    let dir = TestDir::new("reconcile-absent");
    let (mut medium, f) = founded_store(dir.path());
    let never_written = hash_object(b"never-written");
    match medium.reconcile_unknown(&never_written).expect("reconcile") {
        Reconciliation::NotInstalled { head } => assert_eq!(head, Some(f)),
        other => panic!("expected NotInstalled, got {other:?}"),
    }
}

#[test]
fn reconcile_unverifiable_on_injected_fault() {
    let dir = TestDir::new("reconcile-unverifiable");
    let (medium, f) = founded_store(dir.path());
    drop(medium);
    let mut faulted = LocalFileMedium::open_with(
        dir.path(),
        Some(FaultControl::new().arm(FaultPoint::ReconcileLocked, FaultAction::InjectError)),
    )
    .expect("open faulted");
    match faulted.reconcile_unknown(&f).expect("reconcile completes") {
        Reconciliation::Unverifiable { .. } => {}
        other => panic!("expected Unverifiable, got {other:?}"),
    }
}

// ------------------------------------------------- corruption corpus

#[test]
fn truncated_head_fails_closed() {
    let dir = TestDir::new("head-truncated");
    {
        let (_medium, _f) = founded_store(dir.path());
    }
    let head = dir.path().join(HEAD_FILE);
    let bytes = fs::read(&head).expect("read HEAD");
    fs::write(&head, &bytes[..10]).expect("truncate HEAD");

    let error = open_expect_err(dir.path(), "torn HEAD fails closed");
    assert!(
        matches!(error, MediumError::CorruptHead { .. }),
        "expected CorruptHead, got {error}"
    );
    // Never rolled back: the invalid HEAD is still there.
    assert!(head.exists());

    // An empty HEAD is equally corrupt — a valid prefix is never
    // silently promoted.
    fs::write(&head, b"").expect("empty HEAD");
    let error = open_expect_err(dir.path(), "empty HEAD fails closed");
    assert!(matches!(error, MediumError::CorruptHead { .. }));
}

#[test]
fn head_names_missing_object_fails_closed() {
    let dir = TestDir::new("head-missing");
    {
        let (mut medium, f) = founded_store(dir.path());
        let r1 = regular(f, 0, 1);
        let r1_id = append_accepted(&mut medium, &r1, &[], Some(f));
        fs::remove_file(object_path(dir.path(), &r1_id)).expect("remove head object");
    }
    let error = open_expect_err(dir.path(), "missing head object fails closed");
    assert!(
        matches!(error, MediumError::MissingObject { .. }),
        "expected MissingObject, got {error}"
    );
}

#[test]
fn corrupt_record_fails_closed_and_is_preserved() {
    let dir = TestDir::new("corrupt-record");
    let (mut medium, f) = founded_store(dir.path());
    let r1 = regular(f, 0, 1);
    append_accepted(&mut medium, &r1, &[], Some(f));

    // Flip one byte inside the founding record — a record referenced
    // by the head's parent link.
    let path = object_path(dir.path(), &f);
    let mut bytes = fs::read(&path).expect("read record");
    bytes[0] ^= 0xff;
    fs::write(&path, &bytes).expect("corrupt record");

    // A session already open reports the mismatch on a direct read.
    assert!(
        matches!(
            medium.read_record(&f),
            Err(MediumError::HashMismatch { .. })
        ),
        "a direct read reports the hash mismatch"
    );
    drop(medium);

    // A fresh session fails closed at the cold walk.
    let error = open_expect_err(dir.path(), "referenced corruption fails closed");
    assert!(
        matches!(error, MediumError::ReferencedObjectCorrupt { hash } if hash == f),
        "expected ReferencedObjectCorrupt for the founding record, got {error}"
    );
    // Fail-closed and evidence-preserving: the corrupted bytes are
    // still in place — never replaced, never deleted, never
    // quarantined.
    assert_eq!(fs::read(&path).expect("reread"), bytes);
}

#[test]
fn corrupt_content_object_fails_closed_and_is_preserved() {
    let dir = TestDir::new("corrupt-content");
    let (mut medium, f) = founded_store(dir.path());
    let object = content_object(b"payload-c");
    let object_hash = object.hash();
    let r1 = regular(f, 0, 1).with_content_refs(vec![object_hash]);
    append_accepted(&mut medium, &r1, &[object], Some(f));
    drop(medium);

    let path = object_path(dir.path(), &object_hash);
    let mut bytes = fs::read(&path).expect("read content");
    bytes[0] ^= 0xff;
    fs::write(&path, &bytes).expect("corrupt content");

    let error = open_expect_err(dir.path(), "referenced corruption fails closed");
    assert!(
        matches!(error, MediumError::ReferencedObjectCorrupt { .. }),
        "expected ReferencedObjectCorrupt, got {error}"
    );
    assert_eq!(fs::read(&path).expect("reread"), bytes);
}

#[test]
fn epoch_regression_chain_fails_closed() {
    let dir = TestDir::new("chain-epoch");
    let root = dir.path();
    let f = founding(0);
    let f_id = install_object(root, &f.canonical_bytes());
    // Claims epoch 5 over an epoch-0 parent: the linkage check fires.
    let r = regular(f_id, 4, 1);
    let r_id = install_object(root, &r.canonical_bytes());
    install_head(root, &r_id);

    let error = open_expect_err(root, "epoch regression fails closed");
    expect_chain(error, "epoch-regression");
}

#[test]
fn duplicate_keys_chain_fails_closed() {
    let dir = TestDir::new("chain-dup-keys");
    let root = dir.path();
    let f = founding(0).with_idempotency_key(b"shared".to_vec());
    let f_id = install_object(root, &f.canonical_bytes());
    let r = regular(f_id, 0, 1).with_idempotency_key(b"shared".to_vec());
    let r_id = install_object(root, &r.canonical_bytes());
    install_head(root, &r_id);

    let error = open_expect_err(root, "duplicate keys fail closed");
    expect_chain(error, "duplicate-idempotency-key");
}

#[test]
fn self_parenting_corruption_fails_closed_without_hanging() {
    let dir = TestDir::new("chain-self-parent");
    let root = dir.path();
    // An object claiming its own storage name as parent. Its bytes
    // cannot hash to that name (a SHA-256 fixed point), so the walk
    // stops at the hash check — bounded and fail-closed. This is the
    // reachable form of cyclic corruption; the walk's cycle guards
    // are defense in depth behind content addressing.
    let name = hash_object(b"arbitrary-name");
    let record = regular(name, 0, 1);
    let dir_objects = root.join(OBJECTS_DIR);
    fs::create_dir_all(&dir_objects).expect("objects dir");
    fs::write(dir_objects.join(name.hex()), record.canonical_bytes()).expect("install");
    install_head(root, &name);

    let error = open_expect_err(root, "self-parenting fails closed");
    assert!(
        matches!(error, MediumError::ReferencedObjectCorrupt { .. }),
        "expected ReferencedObjectCorrupt, got {error}"
    );
}

#[test]
fn divergent_head_switch_fails_closed() {
    let dir = TestDir::new("divergent-switch");
    let (mut medium, f) = founded_store(dir.path());
    let r1 = regular(f, 0, 1);
    let r1_id = append_accepted(&mut medium, &r1, &[], Some(f));

    // An external actor swaps in a different valid chain (its own
    // founding record under a different campaign id).
    let other = RevisionRecord::founding(AuthorityMode::Standalone, "other-campaign", 0);
    let other_id = install_object(dir.path(), &other.canonical_bytes());
    install_head(dir.path(), &other_id);

    // The session's append must fail closed: the observed history is
    // not a descendant of the trusted one — never silently merged.
    let r2 = regular(r1_id, 1, 2);
    let error = medium
        .append(&r2, &[], Some(r1_id))
        .expect_err("divergence fails closed");
    expect_chain(error, "divergence");
}

#[test]
fn head_removed_fails_closed_divergence() {
    let dir = TestDir::new("head-removed");
    let (mut medium, f) = founded_store(dir.path());
    fs::remove_file(dir.path().join(HEAD_FILE)).expect("remove HEAD");
    let record = regular(f, 0, 1);
    let error = medium
        .append(&record, &[], Some(f))
        .expect_err("vanished HEAD fails closed");
    expect_chain(error, "divergence");
}

#[test]
fn divergent_head_with_missing_object_fails_closed() {
    let dir = TestDir::new("divergent-missing");
    let (mut medium, f) = founded_store(dir.path());
    // HEAD points at a record that was never written.
    let phantom = hash_object(b"phantom");
    install_head(dir.path(), &phantom);
    let record = regular(f, 0, 1);
    let error = medium
        .append(&record, &[], Some(f))
        .expect_err("missing divergent head");
    assert!(
        matches!(error, MediumError::MissingObject { .. }),
        "expected MissingObject, got {error}"
    );
}

// ------------------------------------------------------ cursor behavior

#[test]
fn external_descendant_suffix_absorbed_then_o1() {
    let dir = TestDir::new("suffix-absorb");
    let (mut medium, f) = founded_store(dir.path());

    // A second session appends externally.
    let mut external = LocalFileMedium::open(dir.path()).expect("open external");
    let r2 = regular(f, 0, 1);
    let r2_id = append_accepted(&mut external, &r2, &[], Some(f));
    drop(external);

    // The first session appends against its stale parent: the
    // external suffix is validated and absorbed (bounded walk), then
    // the append is correctly stale-rejected.
    let r3 = regular(f, 0, 2);
    match medium.append(&r3, &[], Some(f)).expect("append completes") {
        AppendOutcome::RejectedStaleParent { observed } => assert_eq!(observed, Some(r2_id)),
        other => panic!("expected stale rejection, got {other:?}"),
    }
    assert_eq!(
        medium.walk_stats().last_append_walked,
        1,
        "exactly the one-record suffix was walked"
    );

    // The next append on the observed head is O(1) — no walk.
    let r4 = regular(r2_id, 1, 3);
    append_accepted(&mut medium, &r4, &[], Some(r2_id));
    assert_eq!(medium.walk_stats().last_append_walked, 0);

    let summary = medium.validate().expect("validate");
    assert_eq!(summary.records, 3);
    assert_eq!(summary.head, r4.id());
}

#[test]
fn cold_walk_counts_and_appends_are_o1() {
    let dir = TestDir::new("o1-cursor");
    {
        let (mut medium, f) = founded_store(dir.path());
        let mut parent = f;
        for tick in 1..=4u64 {
            let record = regular(parent, tick - 1, tick);
            parent = append_accepted(&mut medium, &record, &[], Some(parent));
        }
    }
    let mut medium = LocalFileMedium::open(dir.path()).expect("reopen");
    assert_eq!(medium.walk_stats().cold_walk_records, 5);

    let head = medium.head().expect("head").expect("founded");
    let record = regular(head, 4, 5);
    append_accepted(&mut medium, &record, &[], Some(head));
    assert_eq!(
        medium.walk_stats().last_append_walked,
        0,
        "an append to the verified head walks nothing"
    );

    let summary = medium.validate().expect("validate");
    assert_eq!(summary.records, 6);
    assert_eq!(summary.height, 5);
}

// -------------------------------------------------- orphans and quarantine

#[test]
fn junk_swept_to_quarantine_never_deleted() {
    let dir = TestDir::new("sweep");
    let root = dir.path();
    {
        let (mut medium, f) = founded_store(root);
        let object = content_object(b"payload-s");
        let r1 = regular(f, 0, 1).with_content_refs(vec![object.hash()]);
        append_accepted(&mut medium, &r1, &[object], Some(f));
    }

    // Junk under a valid hash name whose bytes do not hash to it, and
    // junk under a non-hash name. Neither is reachable.
    let junk_hash = hash_object(b"junk-bytes");
    let junk_valid_name = object_path(root, &junk_hash);
    fs::write(&junk_valid_name, b"not-the-hashed-content").expect("place junk");
    let junk_named = root.join(OBJECTS_DIR).join("junk-file.txt");
    fs::write(&junk_named, b"plain junk").expect("place named junk");

    let mut medium = LocalFileMedium::open(root).expect("reopen sweeps");
    let summary = medium.validate().expect("chain still validates");
    assert_eq!(summary.records, 2);

    let quarantine = root.join(QUARANTINE_DIR);
    let swept_valid = quarantine.join(junk_hash.hex());
    assert!(swept_valid.exists(), "hash-named junk quarantined");
    assert_eq!(
        fs::read(&swept_valid).expect("preserved"),
        b"not-the-hashed-content",
        "quarantine preserves bytes — never deletes"
    );
    let swept_named = quarantine.join("junk-file.txt");
    assert!(swept_named.exists(), "non-hash junk quarantined");
    assert_eq!(fs::read(&swept_named).expect("preserved"), b"plain junk");
    assert!(!junk_valid_name.exists() && !junk_named.exists());
    assert!(
        quarantine.join("junk-file.txt.reason").exists(),
        "the quarantine reason note accompanies the evidence"
    );
}

#[test]
fn insert_once_reuses_matching_bytes() {
    let dir = TestDir::new("insert-reuse");
    let (mut medium, f) = founded_store(dir.path());

    // The exact object bytes pre-placed under their hash name.
    let object = content_object(b"payload-reuse");
    let object_hash = object.hash();
    let object_bytes = object.bytes().to_vec();
    fs::write(object_path(dir.path(), &object_hash), &object_bytes).expect("pre-place");

    let record = regular(f, 0, 1).with_content_refs(vec![object_hash]);
    append_accepted(&mut medium, &record, &[object], Some(f));
    assert_eq!(
        fs::read(object_path(dir.path(), &object_hash)).expect("read back"),
        object_bytes,
        "the matching pre-placed object is reused as-is"
    );
    assert!(!quarantine_path(dir.path(), &object_hash).exists());
}

#[test]
fn insert_once_mismatch_unreferenced_quarantined_rewritten() {
    let dir = TestDir::new("insert-mismatch");
    let (mut medium, f) = founded_store(dir.path());

    // Different bytes squatting the new object's hash name — not
    // reachable from the chain.
    let object = content_object(b"payload-rewrite");
    let object_hash = object.hash();
    let object_bytes = object.bytes().to_vec();
    let squatter = object_path(dir.path(), &object_hash);
    fs::write(&squatter, b"squatting-garbage").expect("pre-place garbage");

    let record = regular(f, 0, 1).with_content_refs(vec![object_hash]);
    append_accepted(&mut medium, &record, &[object], Some(f));

    assert_eq!(
        fs::read(quarantine_path(dir.path(), &object_hash)).expect("preserved"),
        b"squatting-garbage",
        "the unreferenced mismatch is quarantined, never deleted"
    );
    assert_eq!(
        fs::read(&squatter).expect("rewritten"),
        object_bytes,
        "the correct bytes now live under the hash name"
    );
}

#[test]
fn insert_once_mismatch_referenced_fails_closed() {
    let dir = TestDir::new("insert-referenced");
    let (mut medium, f) = founded_store(dir.path());

    let object = content_object(b"payload-referenced");
    let object_hash = object.hash();
    let r1 = regular(f, 0, 1).with_content_refs(vec![object_hash]);
    let r1_id = append_accepted(&mut medium, &r1, std::slice::from_ref(&object), Some(f));

    // Corrupt the now chain-referenced object's bytes in place.
    let path = object_path(dir.path(), &object_hash);
    let mut corrupted = object.bytes().to_vec();
    corrupted[0] ^= 0xff;
    fs::write(&path, &corrupted).expect("corrupt referenced object");

    // A new revision referencing the same identity with the correct
    // bytes must fail closed — the corrupted file is evidence.
    let r2 = regular(r1_id, 1, 2).with_content_refs(vec![object_hash]);
    let error = medium
        .append(&r2, &[object], Some(r1_id))
        .expect_err("referenced mismatch fails closed");
    assert!(
        matches!(error, MediumError::ReferencedObjectCorrupt { hash } if hash == object_hash),
        "expected ReferencedObjectCorrupt, got {error}"
    );
    assert_eq!(
        fs::read(&path).expect("preserved"),
        corrupted,
        "the corrupted evidence is never replaced or quarantined"
    );
    assert_eq!(medium.head().expect("head"), Some(r1_id), "HEAD unchanged");
}

// ------------------------------------------- review corrective A–D (regressions)

/// A: an external revision claiming an arbitrary epoch over the
/// trusted head fails closed at the suffix boundary (and the fallback
/// cold walk agrees — no cursor poisoning between attempts).
#[test]
fn a_suffix_boundary_epoch_enforced() {
    let dir = TestDir::new("a-boundary-epoch");
    let (mut stale, f) = founded_store(dir.path());

    // An externally installed revision claiming epoch 47 while
    // pointing at the trusted epoch-0 head — hash-consistent, decode
    // valid, semantically invalid.
    let mut external = regular(f, 0, 1);
    external.epoch = 47;
    install_object(dir.path(), &external.canonical_bytes());
    install_head(dir.path(), &external.id());

    let record = regular(f, 0, 2);
    let error = stale
        .append(&record, &[], Some(f))
        .expect_err("the boundary epoch must fail closed");
    expect_chain(error, "epoch-regression");
    // Fail-closed determinism: a second attempt errs identically.
    let error = stale
        .append(&record, &[], Some(f))
        .expect_err("the boundary epoch must fail closed again");
    expect_chain(error, "epoch-regression");
}

/// A: a dependency naming a nonexistent revision fails the cold walk.
#[test]
fn a_dependency_on_nonexistent_revision_fails_closed() {
    let dir = TestDir::new("a-dep-missing");
    let root = dir.path();
    let f = founding(0);
    let f_id = install_object(root, &f.canonical_bytes());
    let phantom = hash_object(b"no-such-revision");
    let r = regular(f_id, 0, 1).with_depends_on(vec![phantom]);
    let r_id = install_object(root, &r.canonical_bytes());
    install_head(root, &r_id);

    let error = open_expect_err(root, "dependency must resolve");
    expect_chain(error, "depends-on-unresolvable");
}

/// A: a dependency naming an existing-but-unrelated object (not a
/// chain ancestor) fails closed — membership in objects/ is never
/// ancestry.
#[test]
fn a_dependency_on_unrelated_object_fails_closed() {
    let dir = TestDir::new("a-dep-unrelated");
    let root = dir.path();
    let f = founding(0);
    let f_id = install_object(root, &f.canonical_bytes());
    // A standalone hash-consistent record that is NOT part of the
    // authoritative chain.
    let stranger = regular(Hash::ZERO, 0, 9);
    let stranger_id = install_object(root, &stranger.canonical_bytes());
    let r = regular(f_id, 0, 1).with_depends_on(vec![stranger_id]);
    let r_id = install_object(root, &r.canonical_bytes());
    install_head(root, &r_id);

    let error = open_expect_err(root, "unrelated dependency must fail");
    expect_chain(error, "depends-on-unresolvable");
}

/// A: a dependency naming a content hash (reachable, but not a
/// revision) fails closed — dependencies name revisions, never
/// content.
#[test]
fn a_dependency_on_content_hash_fails_closed() {
    let dir = TestDir::new("a-dep-content");
    let root = dir.path();
    let f = founding(0);
    let f_id = install_object(root, &f.canonical_bytes());
    // The record references a content object AND names its hash as a
    // dependency — reachable, but not a revision.
    let object = content_object(b"a-dep-content");
    install_object(root, object.bytes());
    let r = regular(f_id, 0, 1)
        .with_content_refs(vec![object.hash()])
        .with_depends_on(vec![object.hash()]);
    let r_id = install_object(root, &r.canonical_bytes());
    install_head(root, &r_id);

    let error = open_expect_err(root, "content-hash dependency must fail");
    expect_chain(error, "depends-on-unresolvable");
}

/// A: epoch-overflow boundaries fail closed with checked arithmetic —
/// u64::MAX epochs never wrap and never panic.
#[test]
fn a_epoch_overflow_fails_closed() {
    // A head claiming u64::MAX over an epoch-0 parent.
    {
        let dir = TestDir::new("a-overflow-max");
        let root = dir.path();
        let f = founding(0);
        let f_id = install_object(root, &f.canonical_bytes());
        let mut r = regular(f_id, 0, 1);
        r.epoch = u64::MAX;
        let r_id = install_object(root, &r.canonical_bytes());
        install_head(root, &r_id);
        let error = open_expect_err(root, "MAX epoch over epoch 0 must fail");
        expect_chain(error, "epoch-regression");
    }
    // Two records both claiming u64::MAX: the linkage's checked add
    // has no successor — the exact overflow boundary.
    {
        let dir = TestDir::new("a-overflow-pair");
        let root = dir.path();
        let f = founding(0);
        let f_id = install_object(root, &f.canonical_bytes());
        let mut r1 = regular(f_id, 0, 1);
        r1.epoch = u64::MAX;
        let r1_id = install_object(root, &r1.canonical_bytes());
        let mut r2 = regular(r1_id, 0, 2);
        r2.epoch = u64::MAX;
        let r2_id = install_object(root, &r2.canonical_bytes());
        install_head(root, &r2_id);
        let error = open_expect_err(root, "MAX-to-MAX linkage must fail");
        expect_chain(error, "epoch-regression");
    }
}

/// B: a multi-revision external suffix is absorbed with the cursor's
/// head, epoch, and indexes mutually consistent — the stale session
/// appends a valid direct descendant afterwards with no further walk.
#[test]
fn b_multi_revision_suffix_absorbed_consistently() {
    let dir = TestDir::new("b-multi-suffix");
    let (mut stale, f) = founded_store(dir.path());

    // An external session appends two revisions.
    let mut external = LocalFileMedium::open(dir.path()).expect("open external");
    let r1 = regular(f, 0, 1);
    let r1_id = append_accepted(&mut external, &r1, &[], Some(f));
    let r2 = regular(r1_id, 1, 2);
    let r2_id = append_accepted(&mut external, &r2, &[], Some(r1_id));
    drop(external);

    // The stale session still expects f: the complete two-record
    // suffix is validated and absorbed, then the append is
    // stale-rejected.
    let stale_record = regular(f, 0, 3);
    match stale
        .append(&stale_record, &[], Some(f))
        .expect("stale append completes")
    {
        AppendOutcome::RejectedStaleParent { observed } => assert_eq!(observed, Some(r2_id)),
        other => panic!("expected stale rejection, got {other:?}"),
    }
    assert_eq!(
        stale.walk_stats().last_append_walked,
        2,
        "the complete two-record suffix was walked"
    );

    // The cursor is consistent: a direct descendant of r2 at epoch 3
    // is accepted (the cursor's epoch must be 2, not 1) with no
    // further walk.
    let r3 = regular(r2_id, 2, 4);
    let r3_id = append_accepted(&mut stale, &r3, &[], Some(r2_id));
    assert_eq!(
        stale.walk_stats().last_append_walked,
        0,
        "the next ordinary append requires no additional history walk"
    );

    let summary = stale.validate().expect("validate");
    assert_eq!(summary.records, 4);
    assert_eq!(summary.height, 3);
    assert_eq!(summary.head, r3_id);
}

/// C: reconciliation never returns Installed from unverified history —
/// the ancestor candidate's own object bytes are corrupted.
#[test]
fn c_reconcile_ancestor_corrupt_candidate_fails_closed() {
    let dir = TestDir::new("c-corrupt-candidate");
    let (mut medium, f) = founded_store(dir.path());
    let r1 = regular(f, 0, 1);
    let r1_id = append_accepted(&mut medium, &r1, &[], Some(f));
    let r2 = regular(r1_id, 1, 2);
    append_accepted(&mut medium, &r2, &[], Some(r1_id));

    // Corrupt the candidate's own bytes after the session opened.
    let path = object_path(dir.path(), &r1_id);
    let mut bytes = fs::read(&path).expect("read candidate");
    bytes[0] ^= 0xff;
    fs::write(&path, &bytes).expect("corrupt candidate");

    let error = medium
        .reconcile_unknown(&r1_id)
        .expect_err("a corrupt candidate must fail closed");
    assert!(
        matches!(error, MediumError::ReferencedObjectCorrupt { hash } if hash == r1_id),
        "expected ReferencedObjectCorrupt, got {error}"
    );
}

/// C: a content object referenced by the candidate is corrupted —
/// reconciliation fails closed instead of confirming.
#[test]
fn c_reconcile_ancestor_corrupt_candidate_content_fails_closed() {
    let dir = TestDir::new("c-corrupt-candidate-content");
    let (mut medium, f) = founded_store(dir.path());
    let object = content_object(b"c-candidate-content");
    let object_hash = object.hash();
    let r1 = regular(f, 0, 1).with_content_refs(vec![object_hash]);
    let r1_id = append_accepted(&mut medium, &r1, std::slice::from_ref(&object), Some(f));
    let r2 = regular(r1_id, 1, 2);
    append_accepted(&mut medium, &r2, &[], Some(r1_id));

    let path = object_path(dir.path(), &object_hash);
    let mut bytes = fs::read(&path).expect("read content");
    bytes[0] ^= 0xff;
    fs::write(&path, &bytes).expect("corrupt content");

    let error = medium
        .reconcile_unknown(&r1_id)
        .expect_err("corrupt candidate content must fail closed");
    assert!(
        matches!(error, MediumError::ReferencedObjectCorrupt { hash } if hash == object_hash),
        "expected ReferencedObjectCorrupt, got {error}"
    );
}

/// C: a descendant's hash-failing content fails closed — the whole
/// authoritative chain is verified before any classification.
#[test]
fn c_reconcile_ancestor_descendant_corrupt_content_fails_closed() {
    let dir = TestDir::new("c-corrupt-descendant-content");
    let (mut medium, f) = founded_store(dir.path());
    let r1 = regular(f, 0, 1);
    let r1_id = append_accepted(&mut medium, &r1, &[], Some(f));
    let object = content_object(b"c-descendant-content");
    let object_hash = object.hash();
    let r2 = regular(r1_id, 1, 2).with_content_refs(vec![object_hash]);
    append_accepted(&mut medium, &r2, std::slice::from_ref(&object), Some(r1_id));

    let path = object_path(dir.path(), &object_hash);
    let mut bytes = fs::read(&path).expect("read content");
    bytes[0] ^= 0xff;
    fs::write(&path, &bytes).expect("corrupt content");

    let error = medium
        .reconcile_unknown(&r1_id)
        .expect_err("corrupt descendant content must fail closed");
    assert!(
        matches!(error, MediumError::ReferencedObjectCorrupt { hash } if hash == object_hash),
        "expected ReferencedObjectCorrupt, got {error}"
    );
}

/// C: a descendant violating epoch integrity fails closed.
#[test]
fn c_reconcile_ancestor_descendant_epoch_violation_fails_closed() {
    let dir = TestDir::new("c-descendant-epoch");
    let root = dir.path();
    // A session open while the chain is valid.
    let (mut medium, f) = founded_store(root);
    let r1 = regular(f, 0, 1);
    let r1_id = append_accepted(&mut medium, &r1, &[], Some(f));
    let r2 = regular(r1_id, 1, 2);
    let r2_id = append_accepted(&mut medium, &r2, &[], Some(r1_id));

    // Externally replace the head with a hash-consistent record
    // claiming a skipping epoch over r1.
    let mut bad = regular(r1_id, 0, 3);
    bad.epoch = 5;
    let bad_id = install_object(root, &bad.canonical_bytes());
    fs::remove_file(object_path(root, &r2_id)).expect("remove the valid head object");
    install_head(root, &bad_id);

    let error = medium
        .reconcile_unknown(&r1_id)
        .expect_err("epoch-violating history must fail closed");
    expect_chain(error, "epoch-regression");
}

/// C: a descendant with an unresolvable dependency fails closed.
#[test]
fn c_reconcile_ancestor_descendant_dependency_violation_fails_closed() {
    let dir = TestDir::new("c-descendant-dependency");
    let root = dir.path();
    // A session open while the chain is valid.
    let (mut medium, f) = founded_store(root);
    let r1 = regular(f, 0, 1);
    let r1_id = append_accepted(&mut medium, &r1, &[], Some(f));
    let r2 = regular(r1_id, 1, 2);
    let r2_id = append_accepted(&mut medium, &r2, &[], Some(r1_id));

    // Externally replace the head with a hash-consistent record
    // carrying a dependency on a revision that does not exist.
    let phantom = hash_object(b"c-descendant-phantom");
    let bad = regular(r1_id, 1, 3).with_depends_on(vec![phantom]);
    let bad_id = install_object(root, &bad.canonical_bytes());
    fs::remove_file(object_path(root, &r2_id)).expect("remove the valid head object");
    install_head(root, &bad_id);

    let error = medium
        .reconcile_unknown(&r1_id)
        .expect_err("dependency-violating history must fail closed");
    expect_chain(error, "depends-on-unresolvable");
}

/// D2: repeated quarantine of distinct objects under the same
/// original name preserves every incident's bytes and provenance.
#[test]
fn d2_repeated_quarantine_same_name_preserves_both_incidents() {
    let dir = TestDir::new("d2-repeat-quarantine");
    let root = dir.path();
    {
        let (_medium, _f) = founded_store(root);
    }
    // First incident: junk under a valid hash name.
    let junk_hash = hash_object(b"first-incident");
    let objects = root.join(OBJECTS_DIR);
    fs::write(objects.join(junk_hash.hex()), b"first-garbage").expect("place first junk");
    {
        let mut medium = LocalFileMedium::open(root).expect("reopen sweeps");
        medium.validate().expect("chain still validates");
    }
    let quarantine = root.join(QUARANTINE_DIR);
    assert_eq!(
        fs::read(quarantine.join(junk_hash.hex())).expect("first incident preserved"),
        b"first-garbage"
    );

    // Second incident under the same name: the collision-safe
    // placement must not destroy the first evidence.
    fs::write(objects.join(junk_hash.hex()), b"second-garbage").expect("place second junk");
    {
        let mut medium = LocalFileMedium::open(root).expect("reopen sweeps again");
        medium.validate().expect("chain still validates");
    }
    assert_eq!(
        fs::read(quarantine.join(junk_hash.hex())).expect("first incident still preserved"),
        b"first-garbage"
    );
    assert_eq!(
        fs::read(quarantine.join(format!("{}.1", junk_hash.hex())))
            .expect("second incident preserved"),
        b"second-garbage"
    );
    // Provenance notes accompany both incidents.
    assert!(
        quarantine
            .join(format!("{}.reason", junk_hash.hex()))
            .exists()
    );
    assert!(
        quarantine
            .join(format!("{}.1.reason", junk_hash.hex()))
            .exists()
    );
}

/// D2: an interrupted quarantine (bytes moved, note never written)
/// leaves the earlier incident untouched on the next pass.
#[test]
fn d2_quarantine_collision_with_interrupted_note() {
    let dir = TestDir::new("d2-interrupted-note");
    let root = dir.path();
    {
        let (_medium, _f) = founded_store(root);
    }
    let junk_hash = hash_object(b"interrupted-incident");
    let objects = root.join(OBJECTS_DIR);
    let quarantine = root.join(QUARANTINE_DIR);
    // Simulate an interrupted quarantine: bytes moved, no reason note.
    fs::write(objects.join(junk_hash.hex()), b"first-garbage").expect("place junk");
    fs::rename(
        objects.join(junk_hash.hex()),
        quarantine.join(junk_hash.hex()),
    )
    .expect("move without a note");

    // A later incident under the same name must not overwrite the
    // noteless evidence.
    fs::write(objects.join(junk_hash.hex()), b"second-garbage").expect("place second junk");
    {
        let mut medium = LocalFileMedium::open(root).expect("reopen sweeps");
        medium.validate().expect("chain still validates");
    }
    assert_eq!(
        fs::read(quarantine.join(junk_hash.hex())).expect("noteless evidence untouched"),
        b"first-garbage"
    );
    assert_eq!(
        fs::read(quarantine.join(format!("{}.1", junk_hash.hex())))
            .expect("second incident preserved"),
        b"second-garbage"
    );
}

/// D2: a session open with HEAD absent (an interrupted founding
/// attempt) sweeps every object into quarantine — evidence preserved,
/// no founding revision manufactured — and a fresh founding succeeds.
#[test]
fn d2_head_absent_sweep_preserves_evidence() {
    let dir = TestDir::new("d2-head-absent");
    let root = dir.path();
    // An interrupted founding attempt: a founding record's object and
    // a content object exist, but HEAD was never published.
    let f = founding(0);
    let f_id = install_object(root, &f.canonical_bytes());
    let object = content_object(b"d2-orphan-content");
    install_object(root, object.bytes());

    let mut medium = LocalFileMedium::open(root).expect("open sweeps the HEAD-absent store");
    // Everything was unreferenced: moved to quarantine, never deleted.
    let quarantine = root.join(QUARANTINE_DIR);
    assert!(
        quarantine.join(f_id.hex()).exists(),
        "the founding attempt's record is preserved"
    );
    assert!(
        quarantine.join(object.hash().hex()).exists(),
        "the founding attempt's content is preserved"
    );
    assert!(!object_path(root, &f_id).exists());
    assert!(!object_path(root, &object.hash()).exists());
    // No founding revision was manufactured.
    assert_eq!(medium.head().expect("head"), None);

    // A fresh founding attempt starts cleanly and succeeds.
    let fresh = founding(7);
    let fresh_id = append_accepted(&mut medium, &fresh, &[], None);
    let summary = medium.validate().expect("validate");
    assert_eq!(summary.records, 1);
    assert_eq!(summary.head, fresh_id);
}
