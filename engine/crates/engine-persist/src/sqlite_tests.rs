//! The PP-B1 comprehensive SQLite test matrix: the ratified §3.2
//! transaction-outcome table (concurrent writers, BUSY contention,
//! stale parents, duplicate keys, corrupt rows, the fault-injection
//! rollback/commit windows), the reconciliation contract, the
//! `SqliteMedium`/`LocalFileMedium` parity (identical logical
//! campaigns produce equivalent `ChainSummary` results and identical
//! canonical reads), the WAL lifecycle, and the **real-process crash
//! matrix** (a genuine child-process kill before and after `COMMIT`,
//! resolved by SQLite's own WAL recovery on the parent's reopen).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::hash::Hash;
use crate::local::LocalFileMedium;
use crate::medium::{
    AppendOutcome, CanonicalMedium, MediumError, Reconciliation, StoredObject, SyncTier,
    UnknownReason,
};
use crate::record::{AuthorityMode, RevisionRecord};
use crate::sqlite::{
    SqliteFaultAction, SqliteFaultControl, SqliteFaultPoint, SqliteMedium, SqliteMediumConfig,
};

// --------------------------------------------------------------- helpers

fn temp_db(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "s7-pp-b1-matrix-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    path
}

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "s7-pp-b1-matrix-dir-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn cleanup_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

/// A well-formed domain-tagged content object (the Encoder's
/// length-prefixed string form).
fn sample_object(payload: &[u8]) -> StoredObject {
    let mut bytes = Vec::new();
    let domain = "s7/pp-b1/";
    bytes.extend_from_slice(&(domain.len() as u64).to_le_bytes());
    bytes.extend_from_slice(domain.as_bytes());
    bytes.extend_from_slice(payload);
    StoredObject::new(bytes).unwrap()
}

fn founding(campaign: &str) -> RevisionRecord {
    RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0)
}

// ------------------------------------------------- the §3.2 outcome matrix

#[test]
fn pp_b1_concurrent_writers_same_parent_one_accepts_one_stale() {
    let path = temp_db("concurrent");
    let mut a = SqliteMedium::open(&path).unwrap();
    let mut b = SqliteMedium::open(&path).unwrap();
    let founding = founding("concurrent");
    a.append(&founding, &[], None).unwrap();
    // B establishes its cursor over the founded chain (a fresh head
    // read cold-walks it).
    assert_eq!(b.head().unwrap(), Some(founding.id()));

    // A moves the head; B's expected parent is now stale.
    let object_a = sample_object(b"a-payload");
    let a_record =
        RevisionRecord::regular(founding.id(), 0, 11).with_content_refs(vec![object_a.hash()]);
    a.append(&a_record, &[object_a], Some(founding.id())).unwrap();

    let object_b = sample_object(b"b-payload");
    let b_record =
        RevisionRecord::regular(founding.id(), 0, 12).with_content_refs(vec![object_b.hash()]);
    let outcome = b
        .append(&b_record, &[object_b], Some(founding.id()))
        .unwrap();
    // The stale verdict follows the in-transaction committed read —
    // genuinely changed authoritative parent, never a guess.
    assert_eq!(
        outcome,
        AppendOutcome::RejectedStaleParent {
            observed: Some(a_record.id())
        }
    );
    // Only A's revision installed: two records total.
    let summary = b.validate().unwrap();
    assert_eq!(summary.records, 2);
    assert_eq!(summary.head, a_record.id());
    cleanup_db(&path);
}

#[test]
fn pp_b1_busy_timeout_is_typed_contention_never_stale_never_unknown() {
    let path = temp_db("busy");
    let mut writer = SqliteMedium::open(&path).unwrap();
    let founding = founding("busy");
    writer.append(&founding, &[], None).unwrap();

    // A second medium instance with a short busy timeout, its cursor
    // established BEFORE the foreign lock is taken (the open path's
    // schema statements are writes and would themselves contend).
    let quick = SqliteMediumConfig {
        busy_timeout: Duration::from_millis(150),
    };
    let mut subject = SqliteMedium::open_with(&path, &quick, None).unwrap();
    assert_eq!(subject.head().unwrap(), Some(founding.id()));

    // A foreign connection holds the SQLite write lock (BEGIN
    // IMMEDIATE acquires it immediately in WAL mode).
    let foreign = Connection::open(&path).unwrap();
    foreign.execute_batch("BEGIN IMMEDIATE").unwrap();

    let object = sample_object(b"busy-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 20).with_content_refs(vec![object.hash()]);
    let result = subject.append(&record, &[object], Some(founding.id()));
    // The classification: typed contention — no parent comparison
    // ran (never stale), no transaction work began (never unknown).
    assert!(
        matches!(result, Err(MediumError::Contention { .. })),
        "expected contention, got {result:?}"
    );
    // The head is untouched by the contended attempt.
    assert_eq!(writer.head().unwrap(), Some(founding.id()));

    // Release the lock; the same append now succeeds — the failure
    // was contention, not a permanent or state-corrupting error.
    foreign.execute_batch("ROLLBACK").unwrap();
    let object = sample_object(b"busy-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 21).with_content_refs(vec![object.hash()]);
    let accepted = subject.append(&record, &[object], Some(founding.id())).unwrap();
    assert!(matches!(accepted, AppendOutcome::Accepted { .. }));
    drop(foreign);
    cleanup_db(&path);
}

#[test]
fn pp_b1_duplicate_idempotency_key_fails_closed() {
    let path = temp_db("dup-key");
    let mut medium = SqliteMedium::open(&path).unwrap();
    let founding = founding("dup-key");
    medium.append(&founding, &[], None).unwrap();

    let first = RevisionRecord::regular(founding.id(), 0, 30)
        .with_idempotency_key(b"shared-key".to_vec());
    medium.append(&first, &[], Some(founding.id())).unwrap();

    let second = RevisionRecord::regular(first.id(), 1, 31)
        .with_idempotency_key(b"shared-key".to_vec());
    let result = medium.append(&second, &[], Some(first.id()));
    assert!(
        matches!(
            result,
            Err(MediumError::Chain {
                rule: "duplicate-idempotency-key"
            })
        ),
        "expected the duplicate-key rule, got {result:?}"
    );
    // Nothing installed: the chain still has two records.
    assert_eq!(medium.validate().unwrap().records, 2);
    cleanup_db(&path);
}

#[test]
fn pp_b1_content_set_mismatch_fails_before_any_work() {
    let path = temp_db("set-mismatch");
    let mut medium = SqliteMedium::open(&path).unwrap();
    let founding = founding("set-mismatch");
    medium.append(&founding, &[], None).unwrap();

    // The record declares no content refs, but an object is supplied
    // (and vice versa) — the pre-window structural check.
    let object = sample_object(b"orphan-payload");
    let record = RevisionRecord::regular(founding.id(), 0, 40);
    let result = medium.append(&record, &[object], Some(founding.id()));
    assert!(
        matches!(
            result,
            Err(MediumError::Chain {
                rule: "content-set-mismatch"
            })
        ),
        "expected the content-set rule, got {result:?}"
    );
    assert_eq!(medium.head().unwrap(), Some(founding.id()));
    cleanup_db(&path);
}

#[test]
fn pp_b1_mid_transaction_injection_rolls_back_typed() {
    let path = temp_db("mid-tx");
    {
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = founding("mid-tx");
        medium.append(&founding, &[], None).unwrap();
    }
    let fault = SqliteFaultControl::new()
        .arm(SqliteFaultPoint::MidObjectInserts, SqliteFaultAction::InjectError);
    let mut medium = SqliteMedium::open_with(&path, &SqliteMediumConfig::default(), Some(fault))
        .unwrap();
    let founding = founding("mid-tx");
    let object = sample_object(b"mid-tx-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 50).with_content_refs(vec![object.hash()]);
    let result = medium.append(&record, &[object], Some(founding.id()));
    // A mid-transaction failure with a clean rollback: the typed
    // error propagates (nothing installed — verified below).
    assert!(
        matches!(result, Err(MediumError::Sqlite { .. })),
        "expected the injected typed failure, got {result:?}"
    );
    // Nothing installed: the durable head is still the founding, and
    // the full validation passes over the intact chain.
    let summary = medium.validate().unwrap();
    assert_eq!(summary.records, 1);
    assert_eq!(summary.head, founding.id());
    cleanup_db(&path);
}

#[test]
fn pp_b1_rollback_uncertainty_is_unknown_and_reconciles_not_installed() {
    let path = temp_db("rollback-unknown");
    {
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = founding("rollback-unknown");
        medium.append(&founding, &[], None).unwrap();
    }
    // Arm BOTH the mid-transaction failure and the rollback failure:
    // the rollback cannot be completed, so the disposition is
    // genuinely unknown.
    let fault = SqliteFaultControl::new()
        .arm(SqliteFaultPoint::MidObjectInserts, SqliteFaultAction::InjectError)
        .arm(SqliteFaultPoint::AtRollback, SqliteFaultAction::InjectError);
    let mut medium = SqliteMedium::open_with(&path, &SqliteMediumConfig::default(), Some(fault))
        .unwrap();
    let founding = founding("rollback-unknown");
    let object = sample_object(b"rollback-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 60).with_content_refs(vec![object.hash()]);
    let outcome = medium
        .append(&record, &[object], Some(founding.id()))
        .unwrap();
    assert_eq!(
        outcome,
        AppendOutcome::Unknown {
            reason: UnknownReason::RollbackUncertain
        }
    );
    // The uncertainty is resolved by checking the authoritative state
    // — never by guessing: the reconcile walks the durable chain.
    let verdict = medium.reconcile_unknown(&record.id()).unwrap();
    assert_eq!(
        verdict,
        Reconciliation::NotInstalled {
            head: Some(founding.id())
        }
    );
    cleanup_db(&path);
}

#[test]
fn pp_b1_commit_failure_is_unknown_and_reconciles_not_installed() {
    let path = temp_db("commit-unknown");
    {
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = founding("commit-unknown");
        medium.append(&founding, &[], None).unwrap();
    }
    let fault = SqliteFaultControl::new()
        .arm(SqliteFaultPoint::CommitFailure, SqliteFaultAction::InjectError);
    let mut medium = SqliteMedium::open_with(&path, &SqliteMediumConfig::default(), Some(fault))
        .unwrap();
    let founding = founding("commit-unknown");
    let object = sample_object(b"commit-fail-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 70).with_content_refs(vec![object.hash()]);
    let outcome = medium
        .append(&record, &[object], Some(founding.id()))
        .unwrap();
    assert_eq!(
        outcome,
        AppendOutcome::Unknown {
            reason: UnknownReason::CommitUncertain
        }
    );
    let verdict = medium.reconcile_unknown(&record.id()).unwrap();
    assert_eq!(
        verdict,
        Reconciliation::NotInstalled {
            head: Some(founding.id())
        }
    );
    cleanup_db(&path);
}

#[test]
fn pp_b1_post_commit_unknown_reconciles_installed() {
    let path = temp_db("after-commit");
    {
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = founding("after-commit");
        medium.append(&founding, &[], None).unwrap();
    }
    // The commit SUCCEEDS, then the acknowledgment is lost (the
    // post-commit window): the caller sees Unknown; the durable state
    // is installed — the reconcile establishes it from the chain.
    let fault = SqliteFaultControl::new()
        .arm(SqliteFaultPoint::AfterCommit, SqliteFaultAction::InjectError);
    let mut medium = SqliteMedium::open_with(&path, &SqliteMediumConfig::default(), Some(fault))
        .unwrap();
    let founding = founding("after-commit");
    let object = sample_object(b"post-commit-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 80).with_content_refs(vec![object.hash()]);
    let outcome = medium
        .append(&record, &[object], Some(founding.id()))
        .unwrap();
    assert_eq!(
        outcome,
        AppendOutcome::Unknown {
            reason: UnknownReason::CommitUncertain
        }
    );
    let verdict = medium.reconcile_unknown(&record.id()).unwrap();
    assert_eq!(verdict, Reconciliation::Installed { id: record.id() });
    // And the durable head really is the new revision.
    assert_eq!(medium.head().unwrap(), Some(record.id()));
    cleanup_db(&path);
}

#[test]
fn pp_b1_trusted_cursor_appends_do_zero_walk_work() {
    let path = temp_db("cursor-o1");
    let mut medium = SqliteMedium::open(&path).unwrap();
    let founding = founding("cursor-o1");
    medium.append(&founding, &[], None).unwrap();
    // The append to the already-verified head walks zero records (the
    // deterministic counter, never a timing assertion).
    assert_eq!(medium.walk_stats().last_append_walked, 0);
    let object = sample_object(b"cursor-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 90).with_content_refs(vec![object.hash()]);
    medium.append(&record, &[object], Some(founding.id())).unwrap();
    assert_eq!(medium.walk_stats().last_append_walked, 0);
    // A fresh medium's session open walks the full chain once.
    let reopened = SqliteMedium::open(&path).unwrap();
    assert_eq!(reopened.walk_stats().cold_walk_records, 2);
    assert_eq!(reopened.walk_stats().last_append_walked, 0);
    cleanup_db(&path);
}

// ----------------------------------------------------------- corruption

#[test]
fn pp_b1_corrupt_referenced_object_row_fails_closed() {
    let path = temp_db("corrupt-object");
    let object = sample_object(b"corrupt-me-payload");
    {
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = founding("corrupt-object");
        medium.append(&founding, &[], None).unwrap();
        let record =
            RevisionRecord::regular(founding.id(), 0, 100).with_content_refs(vec![object.hash()]);
        medium
            .append(&record, &[object.clone()], Some(founding.id()))
            .unwrap();
    }
    // Tamper the stored bytes under the content identity (a direct
    // SQL write outside the engine — the corruption model).
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
        params![b"tampered-bytes".to_vec(), object.hash().as_bytes()],
    )
    .unwrap();
    drop(raw);

    let medium = SqliteMedium::open(&path);
    assert!(
        matches!(medium, Err(MediumError::ReferencedObjectCorrupt { .. })),
        "the session open fails closed on referenced corruption"
    );
    cleanup_db(&path);
}

#[test]
fn pp_b1_corrupt_head_row_fails_closed() {
    let path = temp_db("corrupt-head");
    {
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = founding("corrupt-head");
        medium.append(&founding, &[], None).unwrap();
    }
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'head'",
        params![vec![1u8, 2u8, 3u8]],
    )
    .unwrap();
    drop(raw);

    let medium = SqliteMedium::open(&path);
    assert!(
        matches!(medium, Err(MediumError::CorruptHead { .. })),
        "a malformed head row fails closed"
    );
    cleanup_db(&path);
}

#[test]
fn pp_b1_unverifiable_chain_never_falsely_confirms() {
    let path = temp_db("unverifiable");
    let object = sample_object(b"unverifiable-payload");
    let record = {
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = founding("unverifiable");
        medium.append(&founding, &[], None).unwrap();
        let record =
            RevisionRecord::regular(founding.id(), 0, 110).with_content_refs(vec![object.hash()]);
        medium
            .append(&record, &[object.clone()], Some(founding.id()))
            .unwrap();
        record
    };
    // Corrupt the referenced object; the reconcile cannot validate the
    // chain — Unverifiable, never a guess in either direction.
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
        params![b"broken".to_vec(), object.hash().as_bytes()],
    )
    .unwrap();
    drop(raw);

    let medium = SqliteMedium::open_with(&path, &SqliteMediumConfig::default(), None);
    assert!(medium.is_err(), "the open itself fails closed");
    // A read-only audit connection sees the same corruption.
    let audit = SqliteMedium::open_read_only(&path);
    assert!(audit.is_err(), "the audit open fails closed too");
    // Even a forced reconcile path cannot validate: the medium never
    // opens, so no false confirmation is possible — the fail-closed
    // open IS the contract's answer to an unverifiable chain.
    let _ = record;
    cleanup_db(&path);
}

// ------------------------------------------------------- reconciliation

#[test]
fn pp_b1_reconcile_installed_for_head_and_ancestors() {
    let path = temp_db("reconcile-installed");
    let mut medium = SqliteMedium::open(&path).unwrap();
    let founding = founding("reconcile-installed");
    medium.append(&founding, &[], None).unwrap();
    let object = sample_object(b"reconcile-payload");
    let middle =
        RevisionRecord::regular(founding.id(), 0, 120).with_content_refs(vec![object.hash()]);
    medium.append(&middle, &[object], Some(founding.id())).unwrap();
    let top = RevisionRecord::regular(middle.id(), 1, 121);
    medium.append(&top, &[], Some(middle.id())).unwrap();

    // The head itself.
    assert_eq!(
        medium.reconcile_unknown(&top.id()).unwrap(),
        Reconciliation::Installed { id: top.id() }
    );
    // A strictly older ancestor (the fully validated chain).
    assert_eq!(
        medium.reconcile_unknown(&founding.id()).unwrap(),
        Reconciliation::Installed { id: founding.id() }
    );
    // A candidate never appended.
    let absent = Hash::from_bytes(&[7u8; 32]).unwrap();
    assert_eq!(
        medium.reconcile_unknown(&absent).unwrap(),
        Reconciliation::NotInstalled { head: Some(top.id()) }
    );
    cleanup_db(&path);
}

// ---------------------------------------------------------- WAL lifecycle

#[test]
fn pp_b1_wal_recovery_on_reopen_after_connection_drop() {
    let path = temp_db("wal-recovery");
    let object = sample_object(b"wal-payload");
    let record = {
        // Drop the medium WITHOUT a checkpoint — the committed frames
        // live in the WAL; the reopen performs SQLite's own recovery.
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = founding("wal-recovery");
        medium.append(&founding, &[], None).unwrap();
        let record =
            RevisionRecord::regular(founding.id(), 0, 130).with_content_refs(vec![object.hash()]);
        medium.append(&record, &[object], Some(founding.id())).unwrap();
        record
    };
    let mut reopened = SqliteMedium::open(&path).unwrap();
    assert_eq!(reopened.head().unwrap(), Some(record.id()));
    let summary = reopened.validate().unwrap();
    assert_eq!(summary.records, 2);
    assert_eq!(summary.head, record.id());
    cleanup_db(&path);
}

#[test]
fn pp_b1_checkpoint_truncate_succeeds_and_validation_survives() {
    let path = temp_db("checkpoint");
    let mut medium = SqliteMedium::open(&path).unwrap();
    let founding = founding("checkpoint");
    medium.append(&founding, &[], None).unwrap();
    let object = sample_object(b"checkpoint-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 140).with_content_refs(vec![object.hash()]);
    medium.append(&record, &[object], Some(founding.id())).unwrap();
    medium.checkpoint_truncate().unwrap();
    // The chain validates identically after the checkpoint.
    assert_eq!(medium.validate().unwrap().records, 2);
    // The reopen still recovers the full state.
    let mut reopened = SqliteMedium::open(&path).unwrap();
    assert_eq!(reopened.head().unwrap(), Some(record.id()));
    cleanup_db(&path);
}

// ----------------------------------------------------------------- parity

#[test]
fn pp_b1_parity_chain_summary_and_reads_match_local_medium() {
    let root = temp_dir("parity");
    let file_root = root.join("file-store");
    let db_path = root.join("store.db");
    let mut file_medium = LocalFileMedium::open(&file_root).unwrap();
    let mut sqlite_medium = SqliteMedium::open(&db_path).unwrap();

    // The identical logical campaign in both providers.
    let campaign = "pp-b1-parity";
    let founding = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0);
    file_medium.append(&founding, &[], None).unwrap();
    sqlite_medium.append(&founding, &[], None).unwrap();

    let mut parent = founding.id();
    let mut objects = Vec::new();
    for step in 0..5u64 {
        let object = sample_object(format!("parity-payload-{step}").as_bytes());
        let record = RevisionRecord::regular(parent, step, 1_000 + step)
            .with_idempotency_key(format!("parity-key-{step}").into_bytes())
            .with_content_refs(vec![object.hash()]);
        file_medium.append(&record, &[object.clone()], Some(parent)).unwrap();
        sqlite_medium.append(&record, &[object.clone()], Some(parent)).unwrap();
        objects.push(object);
        parent = record.id();
    }
    // A dependency-bearing record (the ancestry rules exercised in
    // both walks).
    let dep_source = RevisionRecord::regular(parent, 5, 2_000);
    file_medium.append(&dep_source, &[], Some(parent)).unwrap();
    sqlite_medium.append(&dep_source, &[], Some(parent)).unwrap();
    let dependent = RevisionRecord::regular(dep_source.id(), 6, 2_001)
        .with_depends_on(vec![dep_source.id()]);
    file_medium.append(&dependent, &[], Some(dep_source.id())).unwrap();
    sqlite_medium.append(&dependent, &[], Some(dep_source.id())).unwrap();

    // The ChainSummary parity: identical head, height, founding,
    // authority, campaign, record count, and content-object count.
    let file_summary = file_medium.validate().unwrap();
    let sqlite_summary = sqlite_medium.validate().unwrap();
    assert_eq!(file_summary, sqlite_summary);
    assert_eq!(sqlite_summary.records, 8);

    // The canonical reads round-trip identically.
    let head = sqlite_summary.head;
    let file_record = file_medium.read_record(&head).unwrap();
    let sqlite_record = sqlite_medium.read_record(&head).unwrap();
    assert_eq!(file_record, sqlite_record);
    for object in &objects {
        assert_eq!(
            file_medium.read_object(&object.hash()).unwrap(),
            sqlite_medium.read_object(&object.hash()).unwrap()
        );
    }
    // The durability tiers differ honestly (the platform file tier vs
    // the verified transactional tier) — never conflated.
    let file_tier = file_medium.durability_tier();
    let sqlite_tier = sqlite_medium.durability_tier();
    assert_eq!(sqlite_tier, SyncTier::TransactionalWalFull);
    assert_ne!(file_tier, sqlite_tier);

    let _ = std::fs::remove_dir_all(&root);
}

// ------------------------------------------------- the real-process matrix

/// The child body: re-executed by the parent tests through the test
/// binary with the environment carrying the database path and the
/// armed fault point. Founded unarmed first, then reopened with the
/// armed seam so the fault fires on the REGULAR append (the founding
/// never passes an object-insert phase). With no environment set (the
/// ordinary suite run) this test is a no-op.
#[test]
fn pp_b1_child_append_body() {
    let Ok(db) = std::env::var("S7_PP_B1_CHILD_DB") else {
        return;
    };
    let point_name = std::env::var("S7_PP_B1_CHILD_FAULT").unwrap();
    let point = SqliteFaultPoint::from_name(&point_name).unwrap();
    {
        let mut medium = SqliteMedium::open(&db).unwrap();
        let founding = founding("child-crash");
        medium.append(&founding, &[], None).unwrap();
    }
    let control = SqliteFaultControl::new().arm(point, SqliteFaultAction::Abort);
    let mut medium = SqliteMedium::open_with(
        &db,
        &SqliteMediumConfig::default(),
        Some(control),
    )
    .unwrap();
    let founding = founding("child-crash");
    let object = sample_object(b"child-crash-payload");
    let record =
        RevisionRecord::regular(founding.id(), 0, 200).with_content_refs(vec![object.hash()]);
    let _ = medium.append(&record, &[object], Some(founding.id()));
    // Reaching here means the fault never fired (an error in the
    // harness) — fail loudly.
    panic!("the child should have aborted at {point_name}");
}

/// Spawns the child test binary with the armed fault environment.
fn spawn_child(point: SqliteFaultPoint, db: &Path) -> std::process::ExitStatus {
    let exe = std::env::current_exe().unwrap();
    std::process::Command::new(exe)
        .arg("sqlite_tests::pp_b1_child_append_body")
        .arg("--exact")
        .arg("--nocapture")
        .env("S7_PP_B1_CHILD_DB", db)
        .env("S7_PP_B1_CHILD_FAULT", point.name())
        .status()
        .expect("spawn the child test binary")
}

#[test]
fn pp_b1_child_killed_before_commit_recovers_not_installed() {
    let path = temp_db("kill-before-commit");
    let status = spawn_child(SqliteFaultPoint::ObjectsInserted, &path);
    assert!(!status.success(), "the child must die at the fault point");
    // The parent reopens: SQLite's own WAL recovery discards the
    // uncommitted frames — the durable head is the founding.
    let mut medium = SqliteMedium::open(&path).unwrap();
    let founding = founding("child-crash");
    assert_eq!(medium.head().unwrap(), Some(founding.id()));
    let summary = medium.validate().unwrap();
    assert_eq!(summary.records, 1);
    // The aborted append's candidate reconciles as not installed.
    let object = sample_object(b"child-crash-payload");
    let candidate =
        RevisionRecord::regular(founding.id(), 0, 200).with_content_refs(vec![object.hash()]);
    assert_eq!(
        medium.reconcile_unknown(&candidate.id()).unwrap(),
        Reconciliation::NotInstalled {
            head: Some(founding.id())
        }
    );
    cleanup_db(&path);
}

#[test]
fn pp_b1_child_killed_after_commit_recovers_installed() {
    let path = temp_db("kill-after-commit");
    let status = spawn_child(SqliteFaultPoint::AfterCommit, &path);
    assert!(!status.success(), "the child must die at the fault point");
    // The child died AFTER the successful COMMIT: the WAL recovery
    // replays the committed frames — the durable head is the new
    // revision, and the chain validates.
    let mut medium = SqliteMedium::open(&path).unwrap();
    let founding = founding("child-crash");
    let object = sample_object(b"child-crash-payload");
    let committed =
        RevisionRecord::regular(founding.id(), 0, 200).with_content_refs(vec![object.hash()]);
    assert_eq!(medium.head().unwrap(), Some(committed.id()));
    let summary = medium.validate().unwrap();
    assert_eq!(summary.records, 2);
    assert_eq!(summary.head, committed.id());
    cleanup_db(&path);
}
