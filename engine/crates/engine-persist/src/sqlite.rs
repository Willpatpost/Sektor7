//! `SqliteMedium` — the PP-B1 production canonical medium (the
//! owner-ratified D-PP-1): the accepted [`CanonicalMedium`] contract
//! over **one SQLite database per campaign**, `journal_mode=WAL` with
//! `synchronous=FULL` — **both verified by querying the actual
//! configuration at open** (a request is never assumed granted).
//!
//! **The transaction contract (the ratified §3.2):** one
//! `BEGIN IMMEDIATE` transaction contains every required object
//! insert, the revision-record insert, and the `meta.head` update;
//! **the successful `COMMIT` is the linearization and acceptance
//! boundary** — `Accepted` is returned only after it, carrying the
//! honest [`SyncTier::TransactionalWalFull`] tier. `SQLITE_BUSY` at
//! `BEGIN` is **contention, never a stale parent and never an
//! uncertain commit** (no transaction work began) — the bounded
//! internal retry is SQLite's own busy-timeout schedule, and its
//! expiry is the typed [`MediumError::Contention`] abort. A mid-
//! transaction failure attempts `ROLLBACK`: success is the typed
//! error (nothing installed), a failed rollback is
//! `Unknown{RollbackUncertain}`; a failed `COMMIT` is
//! `Unknown{CommitUncertain}` — never a guessed outcome, reconciled
//! under a fresh committed read. `RejectedStaleParent` is returned
//! only after the head read **inside the transaction** verified a
//! genuinely changed authoritative parent.
//!
//! **`reconcile_unknown`** establishes `Installed` only when the
//! candidate is HEAD or is present in HEAD's **fully validated**
//! ancestor chain; absence from a validated chain is `NotInstalled`;
//! inability to validate is `Unverifiable` — the uncertainty is never
//! converted into a guess, and a corrupt chain never yields a false
//! confirmation.
//!
//! **The trusted cursor** (the §5.3.2 model, identical semantics to
//! `LocalFileMedium`): session-scoped in-memory derived state; O(1)
//! validation work per append to the already-verified head; a bounded
//! suffix walk on external divergence; sound invalidation forces the
//! full cold walk; fail-closed on non-descendant histories. The cold
//! walk enforces every ratified rule: complete record hash
//! verification, checked epoch linkage, chain-wide idempotency-key
//! uniqueness, content-reference integrity, founding coherence, and
//! strictly-earlier dependency ancestry.
//!
//! **No quarantine protocol:** an uncommitted transaction leaves
//! nothing (there are no orphan files); an unreferenced row in
//! `objects` is harmless content-addressed evidence a later retry
//! reuses idempotently; a hash-mismatching row is fail-closed
//! evidence, never rewritten in place. The engine never attempts
//! `.recover` or any destructive repair.
//!
//! **The read-only audit mode** ([`SqliteMedium::open_read_only`]):
//! a `SQLITE_OPEN_READ_ONLY` connection — the WAL snapshot lets the
//! full validation walk run without blocking the writer; `append`
//! refuses with the typed error before any transaction begins.
//!
//! **The checkpoint policy:** SQLite's passive auto-checkpoint runs
//! in operation; [`SqliteMedium::checkpoint_truncate`] issues the
//! ratified `TRUNCATE` checkpoint for the graceful lifecycle
//! boundaries (the session close, the backup capture).
//!
//! **The fault seam** ([`SqliteFaultControl`], test infrastructure):
//! the transaction-phase injection points for the outcome matrix and
//! the real-process crash tests — `Abort` terminates the process at
//! the boundary (a genuine crash), `InjectError` surfaces the phase's
//! failure for the honest classification. A medium constructed
//! without a control never consults the seam.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use engine_core::{Decoder, Encoder};

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

use crate::checkpoint::{
    CHECKPOINT_METADATA_DOMAIN, CheckpointMetadata, ReclaimedIndexEntry, peek_object_domain,
};
use crate::composite_manifest;
use crate::hash::{Hash, hash_object};
use crate::local::WalkStats;
use crate::medium::{
    AppendOutcome, CanonicalMedium, ChainSummary, MediumError, ObjectClass, Reconciliation,
    StoredObject, SyncTier, UnknownReason,
};
use crate::record::{AuthorityMode, RecordKind, RevisionRecord};

/// The `meta` row keys (the design's §3.1 layout).
const META_HEAD: &str = "head";
const META_AUTHORITY: &str = "authority";
#[allow(dead_code)]
const META_PROVENANCE: &str = "provenance";

/// §6.1's migration-target lifecycle (the PP-B1 review corrective):
/// present while the database is an unactivated migration target —
/// the normal opens refuse it; only `open_migration_target`
/// proceeds, and the activation removes it transactionally.
const META_MIGRATION_PENDING: &str = "migration-pending";

/// The ratified schema (the design's §3.1 DDL, `IF NOT EXISTS` for
/// open-time idempotence — two concurrent openers serialize on
/// SQLite's own write lock). The `(32)`-style length annotations of
/// the design are documentation: the insert paths enforce the hash
/// width structurally (`Hash` is exactly 32 bytes).
const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS objects (
  hash   BLOB PRIMARY KEY,
  domain TEXT NOT NULL,
  bytes  BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS records (
  hash   BLOB PRIMARY KEY,
  bytes  BLOB NOT NULL,
  epoch  INTEGER NOT NULL,
  parent BLOB,
  idem   BLOB,
  kind   INTEGER NOT NULL,
  tick   INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS records_idem_unique
  ON records(idem) WHERE idem IS NOT NULL;
CREATE INDEX IF NOT EXISTS records_parent ON records(parent);
CREATE INDEX IF NOT EXISTS records_epoch  ON records(epoch);
";

/// The connection configuration. The busy timeout IS the bounded
/// contention retry (SQLite's internal backoff schedule); its expiry
/// surfaces as the typed [`MediumError::Contention`] abort.
#[derive(Debug, Clone, Copy)]
pub struct SqliteMediumConfig {
    /// How long `BEGIN IMMEDIATE` (and lock-taking statements) retry
    /// internally before surfacing `SQLITE_BUSY`.
    pub busy_timeout: Duration,
}

impl Default for SqliteMediumConfig {
    fn default() -> Self {
        SqliteMediumConfig {
            busy_timeout: Duration::from_millis(5_000),
        }
    }
}

// ------------------------------------------------------------ fault seam

/// The transaction-phase injection points (the SQLite outcome
/// matrix's crash windows). Each maps to a precise point in the
/// append transaction; `Abort` terminates the process there (a
/// genuine crash — the real-process tests), `InjectError` surfaces
/// the phase's failure for the honest classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SqliteFaultPoint {
    /// After the first object insert, further inserts pending.
    MidObjectInserts,
    /// All rows inserted, the `meta.head` update pending.
    ObjectsInserted,
    /// The head updated, the `COMMIT` pending (an injected error here
    /// simulates the commit failure → `Unknown{CommitUncertain}`).
    CommitFailure,
    /// Inside the rollback attempt after a mid-transaction failure
    /// (an injected error here → `Unknown{RollbackUncertain}`).
    AtRollback,
    /// The commit succeeded, before the acknowledgment returns (the
    /// post-commit process-kill window).
    AfterCommit,
}

impl SqliteFaultPoint {
    /// The stable name used by the process-test harness environment.
    pub fn name(&self) -> &'static str {
        match self {
            SqliteFaultPoint::MidObjectInserts => "mid-object-inserts",
            SqliteFaultPoint::ObjectsInserted => "objects-inserted",
            SqliteFaultPoint::CommitFailure => "commit-failure",
            SqliteFaultPoint::AtRollback => "at-rollback",
            SqliteFaultPoint::AfterCommit => "after-commit",
        }
    }

    /// Parses the harness name form.
    pub fn from_name(name: &str) -> Option<Self> {
        const POINTS: &[SqliteFaultPoint] = &[
            SqliteFaultPoint::MidObjectInserts,
            SqliteFaultPoint::ObjectsInserted,
            SqliteFaultPoint::CommitFailure,
            SqliteFaultPoint::AtRollback,
            SqliteFaultPoint::AfterCommit,
        ];
        POINTS.iter().copied().find(|point| point.name() == name)
    }
}

/// What the seam does when it fires at an armed point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteFaultAction {
    /// The process aborts abruptly at the boundary — a real crash
    /// (the OS and SQLite's own locking recover).
    Abort,
    /// The phase's operation surfaces an injected failure; the medium
    /// must classify it honestly.
    InjectError,
}

/// An armed map of point → action. Test-only; constructed via
/// [`SqliteMedium::open_with`].
#[derive(Debug, Clone, Default)]
pub struct SqliteFaultControl {
    armed: BTreeMap<SqliteFaultPoint, SqliteFaultAction>,
}

impl SqliteFaultControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arms a point (builder form).
    pub fn arm(mut self, point: SqliteFaultPoint, action: SqliteFaultAction) -> Self {
        self.armed.insert(point, action);
        self
    }

    fn check(&self, point: SqliteFaultPoint) -> Result<(), MediumError> {
        match self.armed.get(&point) {
            None => Ok(()),
            Some(SqliteFaultAction::Abort) => std::process::abort(),
            Some(SqliteFaultAction::InjectError) => Err(MediumError::Sqlite {
                op: "fault-injected",
                detail: point.name().to_owned(),
            }),
        }
    }
}

// ------------------------------------------------------------- the cursor

/// The session-scoped trusted cursor (the §5.3.2 model — identical
/// semantics to the file medium's).
struct SqliteCursor {
    head: Hash,
    epoch: u64,
    seen_keys: BTreeSet<Vec<u8>>,
    reachable: BTreeSet<Hash>,
    record_ids: BTreeSet<Hash>,
    /// The reclaim-indexed revisions (identity → epoch) from every
    /// on-chain MK's index whose records are genuinely absent — the
    /// historical membership evidence (PP-B3's condition 2). The
    /// observational counts (the MK list, the reclaimed totals) live
    /// on the [`ChainSummary`] the walk returns, never on the cursor.
    reclaimed: BTreeMap<Hash, u64>,
    founding: Hash,
    authority: AuthorityMode,
    campaign_id: String,
    content_objects: u64,
}

/// A completed cold walk: the summary plus the cursor it establishes.
struct WalkedChain {
    summary: ChainSummary,
    cursor: SqliteCursor,
}

// --------------------------------------------------------------- the medium

/// The SQLite-backed production canonical medium.
pub struct SqliteMedium {
    conn: Connection,
    db_path: PathBuf,
    cursor: Option<SqliteCursor>,
    read_only: bool,
    fault: Option<SqliteFaultControl>,
    walks: WalkStats,
}

impl SqliteMedium {
    /// Opens (creating if absent) the campaign database with the
    /// ratified configuration, performing the session open: the
    /// schema creation, the WAL/FULL **verification**, and the cold
    /// founding → HEAD validation that establishes the trusted
    /// cursor.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, MediumError> {
        Self::open_with(path, &SqliteMediumConfig::default(), None)
    }

    /// The full constructor with the configuration and the optional
    /// test fault seam (production passes `None`).
    pub fn open_with(
        path: impl Into<PathBuf>,
        config: &SqliteMediumConfig,
        fault: Option<SqliteFaultControl>,
    ) -> Result<Self, MediumError> {
        let db_path = path.into();
        let conn = Connection::open(&db_path).map_err(|e| sqlite_err("open", e))?;
        configure_writable(&conn, config)?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| sqlite_err_or_contention("create schema", e))?;
        // The unfounded sentinel: an empty head row, seeded once.
        conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('head', x'')",
            [],
        )
        .map_err(|e| sqlite_err_or_contention("seed head row", e))?;
        // §6.1's target lifecycle (the review corrective): a
        // migration-pending database is not a live campaign — the
        // normal open refuses it; only `open_migration_target`
        // proceeds (and the activation removes the marker).
        if let Some(row) = read_meta_row(&conn, META_MIGRATION_PENDING)? {
            decode_migration_pending(&row)?;
            return Err(MediumError::MigrationPending {
                detail: format!(
                    "{} is an unactivated migration target — complete the \
                     migration (a re-run resumes the copy) or remove the \
                     unactivated target; it is not a live campaign",
                    db_path.display()
                ),
            });
        }
        Self::finish_open(conn, db_path, false, fault)
    }

    /// The read-only audit connection: the WAL snapshot lets the full
    /// validation walk run without blocking any writer; `append`
    /// refuses with the typed error before any transaction begins. A
    /// database that cannot be opened or read (including a WAL the
    /// read-only connection cannot map) fails closed with the typed
    /// error — never a silent empty view.
    pub fn open_read_only(path: impl Into<PathBuf>) -> Result<Self, MediumError> {
        let db_path = path.into();
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|e| sqlite_err("open read-only", e))?;
        // journal_mode is persistent in the database header — query
        // (never set) on a read-only connection and verify.
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(|e| sqlite_err("query journal_mode", e))?;
        if mode != "wal" {
            return Err(MediumError::Sqlite {
                op: "verify journal_mode",
                detail: format!("expected wal, observed {mode}"),
            });
        }
        // §6.1's target lifecycle (the review corrective): a pending
        // migration target is not a live campaign — the audit open
        // refuses it too.
        if let Some(row) = read_meta_row(&conn, META_MIGRATION_PENDING)? {
            decode_migration_pending(&row)?;
            return Err(MediumError::MigrationPending {
                detail: format!(
                    "{} is an unactivated migration target — not a live campaign",
                    db_path.display()
                ),
            });
        }
        Self::finish_open(conn, db_path, true, None)
    }

    /// §6.1's target lifecycle (the review corrective): opens the
    /// database AS a migration target — the only constructor that
    /// proceeds past the pending refusal. A fresh unfounded database
    /// receives the persistent `migration-pending` marker (from that
    /// instant it is distinguishable from a standalone campaign, so
    /// an interrupted migration can never be mistaken for one); a
    /// pending target resumes. Anything else refuses: an already
    /// activated target (the provenance row present — a completed
    /// migration is never re-migrated in place) and a founded
    /// standalone campaign (no marker — never hijacked into a
    /// migration).
    pub fn open_migration_target(path: impl Into<PathBuf>) -> Result<Self, MediumError> {
        let db_path = path.into();
        let conn = Connection::open(&db_path).map_err(|e| sqlite_err("open", e))?;
        configure_writable(&conn, &SqliteMediumConfig::default())?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| sqlite_err_or_contention("create schema", e))?;
        conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('head', x'')",
            [],
        )
        .map_err(|e| sqlite_err_or_contention("seed head row", e))?;
        match read_meta_row(&conn, META_MIGRATION_PENDING)? {
            None => {
                if read_meta_row(&conn, META_PROVENANCE)?.is_some() {
                    return Err(MediumError::Sqlite {
                        op: "open_migration_target",
                        detail: "the database is already an activated migration \
                                 target — never re-migrate it in place"
                            .to_owned(),
                    });
                }
                if read_meta_head(&conn)?.is_some() {
                    return Err(MediumError::Sqlite {
                        op: "open_migration_target",
                        detail: "the database is a founded standalone campaign — \
                                 a migration target must be a fresh database"
                            .to_owned(),
                    });
                }
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    params![META_MIGRATION_PENDING, migration_pending_bytes()],
                )
                .map_err(|e| sqlite_err("write migration-pending row", e))?;
            }
            Some(row) => decode_migration_pending(&row)?,
        }
        Self::finish_open(conn, db_path, false, None)
    }

    /// The shared open tail: the head read, the cold walk, the
    /// authority cross-check, and the cursor establishment.
    fn finish_open(
        mut conn: Connection,
        db_path: PathBuf,
        read_only: bool,
        fault: Option<SqliteFaultControl>,
    ) -> Result<Self, MediumError> {
        let tx = conn
            .transaction()
            .map_err(|e| sqlite_err("open read transaction", e))?;
        let observed = read_meta_head(&tx)?;
        let mut walks = WalkStats::default();
        let cursor = match observed {
            Some(head) => {
                let walked = walk_chain(&tx, head)?;
                walks.cold_walk_records = walked.summary.records;
                Some(walked.cursor)
            }
            None => None,
        };
        verify_authority_row(&tx, cursor.as_ref())?;
        tx.commit()
            .map_err(|e| sqlite_err("commit open read transaction", e))?;
        Ok(SqliteMedium {
            conn,
            db_path,
            cursor,
            read_only,
            fault,
            walks,
        })
    }

    /// The database file path (diagnostics and the tooling).
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The campaign identity established by the session open (or a
    /// later validated append) — founding id, authority mode, and
    /// campaign id, without re-walking. `None` before the store is
    /// founded.
    pub fn campaign(&self) -> Option<(Hash, AuthorityMode, &str)> {
        self.cursor.as_ref().map(|cursor| {
            (
                cursor.founding,
                cursor.authority,
                cursor.campaign_id.as_str(),
            )
        })
    }

    /// The deterministic validation-work counters (the O(1)-cursor
    /// test assertions).
    pub fn walk_stats(&self) -> WalkStats {
        self.walks
    }

    /// Test-only (the `composite-test-fixture` feature — never
    /// compiled into a production build): the trusted cursor's
    /// accounting — `(head, epoch, distinct content objects)`. The
    /// cold walk establishes these fields and the hot and suffix
    /// paths maintain them under the same definition, so this is the
    /// direct hot/cold/suffix equivalence observation (a subsequent
    /// `validate()` cold-walks by design and cannot prove it).
    /// `None` before the store is founded.
    #[cfg(feature = "composite-test-fixture")]
    pub fn trusted_cursor_accounting(&self) -> Option<(Hash, u64, u64)> {
        self.cursor
            .as_ref()
            .map(|cursor| (cursor.head, cursor.epoch, cursor.content_objects))
    }

    /// The ratified `TRUNCATE` checkpoint — the graceful lifecycle
    /// boundary operation (the session close, the backup capture).
    /// Returns fail-closed typed errors; never a destructive repair.
    pub fn checkpoint_truncate(&mut self) -> Result<(), MediumError> {
        let (busy, _log, _checkpointed): (i64, i64, i64) = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| sqlite_err("wal_checkpoint(TRUNCATE)", e))?;
        if busy != 0 {
            return Err(MediumError::Contention {
                detail: "the TRUNCATE checkpoint could not obtain the checkpoint lock".to_owned(),
            });
        }
        Ok(())
    }

    /// §6.1's activation (PP-B1, the review corrective — now
    /// transactionally coherent): ONE `BEGIN IMMEDIATE` transaction
    /// that reads the committed head and verifies it equals the
    /// sealed source's head, REQUIRES the `migration-pending` marker
    /// (the target lifecycle's precondition — an activation without
    /// a pending target is an incoherent lifecycle), writes the
    /// provenance row write-once (a contradiction is fail-closed),
    /// and REMOVES the pending marker — the complete
    /// pending→activated transition commits atomically. The caller
    /// owns the seal-first ordering (the legacy `MIGRATED` tombstone
    /// must be verified before this is called).
    pub fn mark_migrated_from(&mut self, source_head: &Hash) -> Result<(), MediumError> {
        let encoded = provenance_bytes(source_head);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| sqlite_err("provenance transaction", e))?;
        // The binding head is verified INSIDE the transaction — the
        // activation cannot interleave with a concurrent head change.
        let observed = read_meta_head(&tx)?;
        if observed != Some(*source_head) {
            return Err(MediumError::Chain { rule: "divergence" });
        }
        // The lifecycle state machine: a FRESH activation requires
        // the pending marker (the target lifecycle's precondition —
        // an activation without a pending target or a completed
        // binding is an incoherent lifecycle); a COMPLETED
        // activation (the provenance row already binding this
        // migration — the interruption between the activation and
        // the archive) is the idempotent no-op.
        let pending = read_meta_row(&tx, META_MIGRATION_PENDING)?;
        match read_meta_row(&tx, META_PROVENANCE)? {
            None => {
                let marker = pending.ok_or_else(|| MediumError::Sqlite {
                    op: "mark_migrated_from",
                    detail: "the database carries no migration-pending marker \
                                 and no completed activation — it is not an \
                                 unactivated migration target"
                        .to_owned(),
                })?;
                decode_migration_pending(&marker)?;
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    params![META_PROVENANCE, encoded],
                )
                .map_err(|e| sqlite_err("insert provenance row", e))?;
                // The lifecycle transition: the pending marker
                // leaves with the activation — one commit.
                tx.execute(
                    "DELETE FROM meta WHERE key = ?1",
                    params![META_MIGRATION_PENDING],
                )
                .map_err(|e| sqlite_err("clear migration-pending row", e))?;
            }
            Some(existing) => {
                if existing != encoded {
                    return Err(MediumError::CorruptHead {
                        detail: "the provenance row contradicts this migration".to_owned(),
                    });
                }
                // The completed activation — idempotent. A leftover
                // pending marker (an impossible mid-transaction state,
                // evidence of tampering) still clears.
                if pending.is_some() {
                    tx.execute(
                        "DELETE FROM meta WHERE key = ?1",
                        params![META_MIGRATION_PENDING],
                    )
                    .map_err(|e| sqlite_err("clear migration-pending row", e))?;
                }
            }
        }
        tx.commit()
            .map_err(|e| sqlite_err("commit provenance row", e))?;
        Ok(())
    }

    /// §6.2's capture (PP-B1): the quiescent backup boundary — fold
    /// the WAL into the main database file (the ratified TRUNCATE
    /// checkpoint; the caller's quiescence contract — no canonical
    /// write, journal write, or rotation in flight across the
    /// capture — makes it uncontended), then `VACUUM INTO` the
    /// target: a complete, self-contained snapshot of the validated
    /// schema and chain. The target must not exist (a prior file is
    /// never quietly overwritten). Online (concurrent-writer) backup
    /// is deferred by design — this is the offline tool's capture.
    pub fn backup_into(&mut self, target: &Path) -> Result<(), MediumError> {
        self.checkpoint_truncate()?;
        self.conn
            .execute(
                "VACUUM INTO ?1",
                params![target.to_string_lossy().to_string()],
            )
            .map_err(|e| sqlite_err("VACUUM INTO", e))?;
        // The captured file must carry the medium's own persistent
        // journal mode: `VACUUM INTO` emits a rollback-journal
        // database, and the read-only audit open verifies WAL (never
        // sets it). Restore the mode on the capture itself — set,
        // then verified by reading it back (never assumed granted);
        // the clean close folds any sidecar away.
        let capture = Connection::open(target).map_err(|e| sqlite_err("open the capture", e))?;
        capture
            .execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| sqlite_err("set the capture's journal mode", e))?;
        let mode: String = capture
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(|e| sqlite_err("verify the capture's journal mode", e))?;
        drop(capture);
        if mode != "wal" {
            return Err(MediumError::Sqlite {
                op: "set the capture's journal mode",
                detail: format!("expected wal, observed {mode}"),
            });
        }
        Ok(())
    }
}

// --------------------------------------------------- the CanonicalMedium impl

impl CanonicalMedium for SqliteMedium {
    fn checkpoint(&mut self) -> Result<(), MediumError> {
        self.checkpoint_truncate()
    }

    fn head(&mut self) -> Result<Option<Hash>, MediumError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| sqlite_err("head read transaction", e))?;
        let observed = read_meta_head(&tx)?;
        validate_observed(&tx, &mut self.cursor, &mut self.walks, observed)?;
        tx.commit()
            .map_err(|e| sqlite_err("commit head read transaction", e))?;
        Ok(observed)
    }

    fn append_admitted(
        &mut self,
        admitted: crate::medium::AdmittedAppend,
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        let (record, objects, composite) = admitted.parts();
        let record = record.clone();
        let objects: Vec<StoredObject> = objects.to_vec();
        if self.read_only {
            return Err(MediumError::Sqlite {
                op: "append",
                detail: "the connection is read-only (the audit mode)".to_owned(),
            });
        }
        // Structural validation (pre-window; a failure is a clean,
        // retryable error — nothing has been written): the record must
        // be canonically representable and reference exactly the
        // supplied object set. Identical to the file medium's checks.
        let record_bytes = record.canonical_bytes();
        let reparsed = RevisionRecord::decode(&record_bytes).map_err(MediumError::Format)?;
        if reparsed != record {
            return Err(MediumError::Chain {
                rule: "record-noncanonical",
            });
        }
        let record_id = hash_object(&record_bytes);
        let supplied: BTreeSet<Hash> = objects.iter().map(StoredObject::hash).collect();
        let declared: BTreeSet<Hash> = record.content_refs.iter().copied().collect();
        if !composite && supplied != declared {
            return Err(MediumError::Chain {
                rule: "content-set-mismatch",
            });
        }
        if composite && !supplied.is_superset(&declared) {
            return Err(MediumError::Chain {
                rule: "composite-content-missing",
            });
        }

        // The transaction: BEGIN IMMEDIATE — the write lock. BUSY
        // here is contention (the bounded busy-timeout retry expired):
        // a typed abort — never a stale parent (no comparison ran),
        // never an uncertain commit (no work began).
        let tx = match self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
        {
            Ok(tx) => tx,
            Err(error) if is_busy(&error) => {
                return Err(MediumError::Contention {
                    detail: format!(
                        "the write lock was unavailable within the busy timeout at {}",
                        self.db_path.display()
                    ),
                });
            }
            Err(error) => return Err(sqlite_err("begin immediate", error)),
        };

        // The observed head, read under the transaction (a committed
        // read — the basis for every stale classification), then the
        // trusted-cursor validation of it.
        let observed = read_meta_head(&tx)?;
        if let Err(error) = validate_observed(&tx, &mut self.cursor, &mut self.walks, observed) {
            let _ = tx.rollback();
            return Err(error);
        }

        // The stale comparison — only after the in-transaction read.
        if observed != expected_parent {
            let _ = tx.rollback();
            return Ok(AppendOutcome::RejectedStaleParent { observed });
        }

        // The record-level coherence checks (against the validated
        // cursor — identical rules to the file medium's).
        match (observed, &record.kind) {
            (None, RecordKind::Founding { .. }) => {
                if record.epoch != 0 || record.parent.is_some() {
                    let _ = tx.rollback();
                    return Err(MediumError::Chain {
                        rule: "kind-incoherent",
                    });
                }
                if !record.depends_on.is_empty() {
                    let _ = tx.rollback();
                    return Err(MediumError::Chain {
                        rule: "depends-on-unresolvable",
                    });
                }
            }
            (Some(head), RecordKind::Regular) => {
                let Some(cursor) = self.cursor.as_ref() else {
                    unreachable!("validation established the cursor for a founded head")
                };
                // Checked arithmetic: an epoch at u64::MAX has no
                // valid successor and must fail closed, never wrap.
                if cursor.epoch.checked_add(1) != Some(record.epoch) {
                    let _ = tx.rollback();
                    return Err(MediumError::Chain {
                        rule: "epoch-regression",
                    });
                }
                if record.parent != Some(head) {
                    let _ = tx.rollback();
                    return Err(MediumError::Chain {
                        rule: "parent-mismatch",
                    });
                }
                if let Some(key) = &record.idempotency_key
                    && cursor.seen_keys.contains(key)
                {
                    let _ = tx.rollback();
                    return Err(MediumError::Chain {
                        rule: "duplicate-idempotency-key",
                    });
                }
                for dependency in &record.depends_on {
                    let resolves = cursor.record_ids.contains(dependency)
                        || cursor
                            .reclaimed
                            .get(dependency)
                            .is_some_and(|&dep_epoch| dep_epoch < record.epoch);
                    if !resolves {
                        let _ = tx.rollback();
                        return Err(MediumError::Chain {
                            rule: "depends-on-unresolvable",
                        });
                    }
                }
            }
            (observed, _) => {
                let _ = tx.rollback();
                return Ok(AppendOutcome::RejectedStaleParent { observed });
            }
        }

        // The K2-C2A PUBLICATION FENCE: revalidate every REUSED
        // composite dependency inside the write transaction,
        // immediately before the inserts — the admission's
        // caller-side reads are advisory; a dependency that vanished
        // (a concurrent connection's committed delete) or changed
        // between the admission and this publication REFUSES typed,
        // with the transaction rolled back and nothing installed.
        // The loader is the transaction-scoped hash-verifying read
        // the walks use — no public storage operation, no nested
        // transaction.
        if composite {
            let supplied: BTreeSet<Hash> = objects.iter().map(StoredObject::hash).collect();
            let fenced = crate::composite_append::fence_reused_dependencies(
                objects[0].bytes(),
                &supplied,
                |identity| load_referenced_object(&tx, identity),
            );
            if let Err(error) = fenced {
                let _ = tx.rollback();
                return Err(error);
            }
        }

        // The inserts: every object (insert-once, verify-on-retry),
        // then the record row. An in-window failure takes the rollback
        // path: a clean rollback is the typed error (nothing
        // installed); a failed rollback is Unknown.
        let transactional = append_inserts(
            &tx,
            &self.cursor,
            &record,
            &objects,
            &record_bytes,
            &record_id,
            &self.fault,
        );
        if let Err(error) = transactional {
            return rollback_or_unknown(tx, &self.fault, error);
        }

        // The head update — the transaction's final write.
        let updated = tx
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'head'",
                params![record_id.as_bytes()],
            )
            .map_err(|e| sqlite_err("update head", e));
        match updated {
            Ok(1) => {}
            Ok(_) => {
                let error = MediumError::CorruptHead {
                    detail: "the head row vanished inside the transaction".to_owned(),
                };
                return rollback_or_unknown(tx, &self.fault, error);
            }
            Err(error) => return rollback_or_unknown(tx, &self.fault, error),
        }

        // The commit — the linearization and acceptance boundary. A
        // failure (including the injected one) is Unknown: the
        // disposition is established only by reconcile_unknown.
        if let Err(injected) = fault_check(&self.fault, SqliteFaultPoint::CommitFailure) {
            let _ = tx.rollback();
            let _ = injected;
            return Ok(AppendOutcome::Unknown {
                reason: UnknownReason::CommitUncertain,
            });
        }
        if let Err(error) = tx.commit() {
            let _ = error;
            return Ok(AppendOutcome::Unknown {
                reason: UnknownReason::CommitUncertain,
            });
        }

        // The post-commit window (the real-process kill point).
        if let Err(error) = fault_check(&self.fault, SqliteFaultPoint::AfterCommit) {
            let _ = error;
            return Ok(AppendOutcome::Unknown {
                reason: UnknownReason::CommitUncertain,
            });
        }

        // The trusted cursor advances over the accepted revision —
        // O(1) work; the R12 corrective carries the VERIFIED
        // composite closure (the manifest's identities — the
        // admission already verified every chunk; the manifest
        // re-decode here is the same bounded object).
        if composite {
            let manifest =
                composite_manifest::decode(objects[0].bytes()).map_err(MediumError::Format)?;
            advance_cursor(
                &mut self.cursor,
                &record,
                &record_id,
                Some(&manifest.closure_identities()),
            );
        } else {
            advance_cursor(&mut self.cursor, &record, &record_id, None);
        }
        Ok(AppendOutcome::Accepted {
            id: record_id,
            tier: SyncTier::TransactionalWalFull,
        })
    }

    fn read_record(&mut self, id: &Hash) -> Result<RevisionRecord, MediumError> {
        let bytes = load_record_verified(&self.conn, id)?;
        RevisionRecord::decode(&bytes).map_err(MediumError::Format)
    }

    fn read_object(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
        load_object_verified(&self.conn, hash)
    }

    fn classify_object(&mut self, hash: &Hash) -> Result<ObjectClass, MediumError> {
        // The SAME full verified read as read_object — the complete
        // object bytes are loaded and hash-checked against the
        // identity (a missing object fails MissingObject, a corrupted
        // object fails HashMismatch); only the small classification
        // crosses back to the caller, eliminating the byte transfer
        // without eliminating the verification.
        let bytes = load_object_verified(&self.conn, hash)?;
        Ok(ObjectClass::of(&bytes))
    }

    fn validate(&mut self) -> Result<ChainSummary, MediumError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| sqlite_err("validate read transaction", e))?;
        let observed = read_meta_head(&tx)?;
        let Some(head) = observed else {
            let _ = tx.rollback();
            return Err(MediumError::Chain { rule: "unfounded" });
        };
        // Sound invalidation first: the full walk rebuilds the cursor
        // from the durable truth alone.
        self.cursor = None;
        let walked = walk_chain(&tx, head)?;
        let summary = walked.summary;
        self.walks.cold_walk_records = summary.records;
        self.cursor = Some(walked.cursor);
        tx.commit()
            .map_err(|e| sqlite_err("commit validate read transaction", e))?;
        Ok(summary)
    }

    fn reconcile_unknown(&mut self, candidate: &Hash) -> Result<Reconciliation, MediumError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|e| sqlite_err("reconcile read transaction", e))?;
        let observed = read_meta_head(&tx)?;
        let result = match observed {
            Some(head) => {
                // Full verification before any classification: the
                // cold walk establishes a fully valid authoritative
                // chain. Installed only when the candidate is HEAD or
                // a member of the walked revision-id set — a matching
                // parent hash alone is never proof. An inability to
                // validate is Unverifiable (never a false
                // confirmation, never a guess).
                match walk_chain(&tx, head) {
                    Ok(walked) => {
                        self.walks.last_reconcile_walked = walked.summary.records;
                        if head == *candidate || walked.cursor.record_ids.contains(candidate) {
                            Reconciliation::Installed { id: *candidate }
                        } else {
                            // Not installed in a verified chain: any
                            // candidate row is unreferenced
                            // content-addressed evidence — retained
                            // (never deleted), reused idempotently by
                            // an honest retry.
                            Reconciliation::NotInstalled { head: Some(head) }
                        }
                    }
                    Err(error) => Reconciliation::Unverifiable {
                        reason: error.to_string(),
                    },
                }
            }
            None => Reconciliation::NotInstalled { head: None },
        };
        tx.commit()
            .map_err(|e| sqlite_err("commit reconcile read transaction", e))?;
        Ok(result)
    }

    fn durability_tier(&self) -> SyncTier {
        SyncTier::TransactionalWalFull
    }

    fn reclaim(
        &mut self,
        directive: &crate::reclaim::ReclaimDirective,
    ) -> Result<crate::reclaim::ReclaimOutcome, MediumError> {
        // THE TRUSTED BOUNDARY (K2-C2B P2): ONE write-serialized
        // transaction covers the authentication, the partition, and
        // the deletion — the decision and its destructive
        // consequences are atomic (an interruption is the full
        // rollback: nothing deleted, the retry from scratch; the
        // manifest-driven convergence holds trivially).
        if self.read_only {
            return Err(MediumError::Sqlite {
                op: "reclaim",
                detail: "the connection is read-only (the audit mode)".to_owned(),
            });
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| sqlite_err("begin reclaim transaction", e))?;
        let outcome = reclaim_authorized_tx(&tx, directive)?;
        tx.commit()
            .map_err(|e| sqlite_err("commit reclaim transaction", e))?;
        Ok(outcome)
    }

    fn reclaim_pending(
        &mut self,
        directive: &crate::reclaim::ReclaimDirective,
    ) -> Result<bool, MediumError> {
        // The Mandatory-only probe (the advisory read over the plain
        // connection — the same logical authority contract).
        reclaim_pending_authorized(&self.conn, directive)
    }
}

/// The trusted reclamation internals (K2-C2B P2) — the transaction-
/// scoped authentication and execution (the same logical authority
/// contract as the local provider's locked form).
fn reclaim_authorized_tx(
    tx: &rusqlite::Transaction<'_>,
    directive: &crate::reclaim::ReclaimDirective,
) -> Result<crate::reclaim::ReclaimOutcome, MediumError> {
    let (ledger, _metadata, head) = authenticate_audit_tx(tx, &directive.audit_record)?;
    if ledger.units != directive.units {
        return Err(MediumError::Chain {
            rule: "reclaim/directive-units-mismatch",
        });
    }
    // The manifest reread with the resume tolerance (the records-
    // table read; an absent retiring record is the protocol-
    // accounted resume).
    for unit in &ledger.units {
        if record_exists(tx, &unit.record)? {
            let bytes = load_record_verified(tx, &unit.record)?;
            let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
            if unit.objects != record.content_refs {
                return Err(MediumError::Chain {
                    rule: "reclaim/units-objects-mismatch",
                });
            }
        }
    }
    // The survivor partition under the same transaction.
    let exclusion: BTreeSet<Hash> = ledger.units.iter().map(|unit| unit.record).collect();
    let walked = walk_chain_excluding(tx, head, &exclusion)?;
    let survivors = walked.cursor.reachable;
    let mut partition = crate::reclaim::partition_reclaim(&ledger.units, &survivors)?;
    let mut removed = 0u64;
    // PASS 1 — the retiring records, newest-first (the ledger's unit
    // order), idempotent.
    for unit in &ledger.units {
        let count = tx
            .execute(
                "DELETE FROM records WHERE hash = ?1",
                params![unit.record.as_bytes()],
            )
            .map_err(|e| sqlite_err("delete record", e))?;
        removed += count as u64;
    }
    // PASS 2 — the deduplicated MANDATORY objects only (the deferred
    // identities never touched); the row deletion is the audited
    // identity's own (the hash-verified reads during the
    // authentication established the integrity).
    for identity in &partition.mandatory {
        let count = tx
            .execute(
                "DELETE FROM objects WHERE hash = ?1",
                params![identity.as_bytes()],
            )
            .map_err(|e| sqlite_err("delete object", e))?;
        removed += count as u64;
    }
    partition.removed = removed;
    Ok(partition)
}

/// The transaction-scoped canonical audit authentication (the same
/// discipline as the local provider's locked form: the exact audit
/// identity via the hash-verified load, the MK from the decoded
/// ledger, the deterministic key, the requested MK's metadata bound
/// by revision identity, the bindings, and the units-vs-index
/// membership — the recovery-safe form without the per-unit
/// reread).
fn authenticate_audit_tx(
    tx: &rusqlite::Transaction<'_>,
    audit_record: &Hash,
) -> Result<
    (
        crate::reclaim::ReclamationLedger,
        crate::checkpoint::CheckpointMetadata,
        Hash,
    ),
    MediumError,
> {
    authenticate_audit_on(tx, audit_record)
}

#[allow(dead_code)]
fn authenticate_audit_tx_unused(
    tx: &rusqlite::Transaction<'_>,
    audit_record: &Hash,
) -> Result<
    (
        crate::reclaim::ReclamationLedger,
        crate::checkpoint::CheckpointMetadata,
        Hash,
    ),
    MediumError,
> {
    const MAX_DESCENT: u64 = 1 << 20;
    let mut current = read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
    let head = current;
    let mut ledger: Option<crate::reclaim::ReclamationLedger> = None;
    let mut metadata: Option<crate::checkpoint::CheckpointMetadata> = None;
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    for _ in 0..MAX_DESCENT {
        let bytes = load_record_verified(tx, &current)?;
        let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
        let parent = record.parent;
        if let Some(object) = record.content_refs.first().copied() {
            let object_bytes = load_object_verified(tx, &object)?;
            let domain = StoredObject::new(object_bytes.clone())?.domain().to_owned();
            if domain == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN {
                let decoded = crate::checkpoint::CheckpointMetadata::decode(&object_bytes)
                    .map_err(MediumError::Format)?;
                skip_table = Some(decoded.clone());
                if let Some(ledger) = ledger.as_ref()
                    && current == ledger.mk
                {
                    if decoded.sk != parent.unwrap_or(Hash::ZERO) {
                        return Err(MediumError::Chain {
                            rule: "reclaim/mk-binding-mismatch",
                        });
                    }
                    metadata = Some(decoded);
                }
            } else if domain == crate::reclaim::RECLAMATION_LEDGER_DOMAIN
                && current == *audit_record
            {
                let decoded = crate::reclaim::ReclamationLedger::decode(&object_bytes)
                    .map_err(MediumError::Format)?;
                let expected = crate::reclaim::reclamation_ledger_idempotency_key(&decoded.mk);
                if record.idempotency_key != Some(expected) {
                    return Err(MediumError::Chain {
                        rule: "reclaim/audit-key-mismatch",
                    });
                }
                ledger = Some(decoded);
            }
        }
        if ledger.is_some() && metadata.is_some() {
            break;
        }
        let Some(parent) = parent else {
            break;
        };
        if record_exists(tx, &parent)? {
            current = parent;
        } else {
            let Some(table) = skip_table.as_ref() else {
                return Err(MediumError::MissingObject { hash: parent });
            };
            let target = if table.sk == parent {
                table
                    .self_skip()
                    .map(|entry| entry.nearest_retained_ancestor)
            } else {
                table
                    .skip_for(&current)
                    .map(|entry| entry.nearest_retained_ancestor)
            };
            let Some(target) = target else {
                return Err(MediumError::MissingObject { hash: parent });
            };
            current = target;
        }
    }
    let Some(ledger) = ledger else {
        return Err(MediumError::Chain {
            rule: "reclaim/audit-not-canonical",
        });
    };
    let Some(metadata) = metadata else {
        return Err(MediumError::Chain {
            rule: "reclaim/mk-not-a-checkpoint",
        });
    };
    if ledger.sk != metadata.sk
        || ledger.range_digest != metadata.range_digest
        || ledger.seed != metadata.seed
    {
        return Err(MediumError::Chain {
            rule: "reclaim/binding-mismatch",
        });
    }
    let index: BTreeSet<Hash> = metadata.index.iter().map(|entry| entry.hash).collect();
    let units: BTreeSet<Hash> = ledger.units.iter().map(|unit| unit.record).collect();
    if index.len() != metadata.index.len() || units.len() != ledger.units.len() || index != units {
        return Err(MediumError::Chain {
            rule: "reclaim/units-index-mismatch",
        });
    }
    Ok((ledger, metadata, head))
}

/// The Mandatory-only probe over the plain connection (the advisory
/// read; the same authentication and partition).
fn reclaim_pending_authorized(
    conn: &Connection,
    directive: &crate::reclaim::ReclaimDirective,
) -> Result<bool, MediumError> {
    let (ledger, _metadata, head) = authenticate_audit_conn(conn, &directive.audit_record)?;
    if ledger.units != directive.units {
        return Err(MediumError::Chain {
            rule: "reclaim/directive-units-mismatch",
        });
    }
    let exclusion: BTreeSet<Hash> = ledger.units.iter().map(|unit| unit.record).collect();
    let walked = walk_chain_excluding(conn, head, &exclusion)?;
    let survivors = walked.cursor.reachable;
    let partition = crate::reclaim::partition_reclaim(&ledger.units, &survivors)?;
    for identity in &partition.mandatory {
        let present: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE hash = ?1 UNION ALL                  SELECT 1 FROM records WHERE hash = ?1 LIMIT 1)",
                params![identity.as_bytes()],
                |row| row.get(0),
            )
            .map_err(|e| sqlite_err("probe mandatory identity", e))?;
        if present != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The plain-connection variant of the audit authentication (the
/// probe path — the same checks over the unlocked reads).
fn authenticate_audit_conn(
    conn: &Connection,
    audit_record: &Hash,
) -> Result<
    (
        crate::reclaim::ReclamationLedger,
        crate::checkpoint::CheckpointMetadata,
        Hash,
    ),
    MediumError,
> {
    // The identical discipline over the plain connection: reuse the
    // transaction form through the connection's own implicit read
    // consistency (SQLite's connection-scoped snapshot).
    authenticate_audit_on(conn, audit_record)
}

/// The shared descent over ANY connection or transaction handle.
fn authenticate_audit_on(
    conn: &Connection,
    audit_record: &Hash,
) -> Result<
    (
        crate::reclaim::ReclamationLedger,
        crate::checkpoint::CheckpointMetadata,
        Hash,
    ),
    MediumError,
> {
    const MAX_DESCENT: u64 = 1 << 20;
    let mut current = read_meta_head(conn)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
    let head = current;
    let mut ledger: Option<crate::reclaim::ReclamationLedger> = None;
    let mut metadata: Option<crate::checkpoint::CheckpointMetadata> = None;
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    for _ in 0..MAX_DESCENT {
        let bytes = load_record_verified(conn, &current)?;
        let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
        let parent = record.parent;
        if let Some(object) = record.content_refs.first().copied() {
            let object_bytes = load_object_verified(conn, &object)?;
            let domain = StoredObject::new(object_bytes.clone())?.domain().to_owned();
            if domain == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN {
                let decoded = crate::checkpoint::CheckpointMetadata::decode(&object_bytes)
                    .map_err(MediumError::Format)?;
                skip_table = Some(decoded.clone());
                if let Some(ledger) = ledger.as_ref()
                    && current == ledger.mk
                {
                    if decoded.sk != parent.unwrap_or(Hash::ZERO) {
                        return Err(MediumError::Chain {
                            rule: "reclaim/mk-binding-mismatch",
                        });
                    }
                    metadata = Some(decoded);
                }
            } else if domain == crate::reclaim::RECLAMATION_LEDGER_DOMAIN
                && current == *audit_record
            {
                let decoded = crate::reclaim::ReclamationLedger::decode(&object_bytes)
                    .map_err(MediumError::Format)?;
                let expected = crate::reclaim::reclamation_ledger_idempotency_key(&decoded.mk);
                if record.idempotency_key != Some(expected) {
                    return Err(MediumError::Chain {
                        rule: "reclaim/audit-key-mismatch",
                    });
                }
                ledger = Some(decoded);
            }
        }
        if ledger.is_some() && metadata.is_some() {
            break;
        }
        let Some(parent) = parent else {
            break;
        };
        if record_exists(conn, &parent)? {
            current = parent;
        } else {
            let Some(table) = skip_table.as_ref() else {
                return Err(MediumError::MissingObject { hash: parent });
            };
            let target = if table.sk == parent {
                table
                    .self_skip()
                    .map(|entry| entry.nearest_retained_ancestor)
            } else {
                table
                    .skip_for(&current)
                    .map(|entry| entry.nearest_retained_ancestor)
            };
            let Some(target) = target else {
                return Err(MediumError::MissingObject { hash: parent });
            };
            current = target;
        }
    }
    let Some(ledger) = ledger else {
        return Err(MediumError::Chain {
            rule: "reclaim/audit-not-canonical",
        });
    };
    let Some(metadata) = metadata else {
        return Err(MediumError::Chain {
            rule: "reclaim/mk-not-a-checkpoint",
        });
    };
    if ledger.sk != metadata.sk
        || ledger.range_digest != metadata.range_digest
        || ledger.seed != metadata.seed
    {
        return Err(MediumError::Chain {
            rule: "reclaim/binding-mismatch",
        });
    }
    let index: BTreeSet<Hash> = metadata.index.iter().map(|entry| entry.hash).collect();
    let units: BTreeSet<Hash> = ledger.units.iter().map(|unit| unit.record).collect();
    if index.len() != metadata.index.len() || units.len() != ledger.units.len() || index != units {
        return Err(MediumError::Chain {
            rule: "reclaim/units-index-mismatch",
        });
    }
    Ok((ledger, metadata, head))
}

// ------------------------------------------------------------ the inserts
// ------------------------------------------------------------ the inserts

/// The transaction's insert phases: every object insert-once with
/// verify-on-retry, the fault windows, and the record row.
fn append_inserts(
    tx: &rusqlite::Transaction<'_>,
    cursor: &Option<SqliteCursor>,
    record: &RevisionRecord,
    objects: &[StoredObject],
    record_bytes: &[u8],
    record_id: &Hash,
    fault: &Option<SqliteFaultControl>,
) -> Result<(), MediumError> {
    for (index, object) in objects.iter().enumerate() {
        if index == 0 {
            fault_check(fault, SqliteFaultPoint::MidObjectInserts)?;
        }
        insert_object_once(tx, cursor, object)?;
    }
    fault_check(fault, SqliteFaultPoint::ObjectsInserted)?;
    insert_record_once(tx, record, record_bytes, record_id)?;
    // The founding authority row — set once at founding, never
    // updated (the design's §3.1 metadata contract).
    if record.parent.is_none()
        && let RecordKind::Founding {
            authority,
            campaign_id,
        } = &record.kind
    {
        let encoded = authority_bytes(record_id, *authority, campaign_id);
        let present: Option<Vec<u8>> = tx
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![META_AUTHORITY],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(no_rows_as_none)
            .map_err(|e| sqlite_err("read authority row", e))?;
        match present {
            None => {
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    params![META_AUTHORITY, encoded],
                )
                .map_err(|e| sqlite_err("insert authority row", e))?;
            }
            Some(existing) => {
                if existing != encoded {
                    return Err(MediumError::CorruptHead {
                        detail: "the authority row contradicts the founding record".to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Insert-once with verify-on-retry: an existing row is accepted only
/// if its stored bytes hash to the expected identity; a mismatch is
/// fail-closed corruption (referenced when the cursor reaches the
/// identity, otherwise the stored-row mismatch) — the row is never
/// rewritten (it is the evidence; SQLite is never modified in place
/// outside transactions).
fn insert_object_once(
    tx: &rusqlite::Transaction<'_>,
    cursor: &Option<SqliteCursor>,
    object: &StoredObject,
) -> Result<(), MediumError> {
    let hash = object.hash();
    let existing: Option<Vec<u8>> = tx
        .query_row(
            "SELECT bytes FROM objects WHERE hash = ?1",
            params![hash.as_bytes()],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(no_rows_as_none)
        .map_err(|e| sqlite_err("read existing object", e))?;
    match existing {
        Some(bytes) => {
            if hash_object(&bytes) != hash {
                let referenced = cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor.reachable.contains(&hash));
                return Err(if referenced {
                    MediumError::ReferencedObjectCorrupt { hash }
                } else {
                    MediumError::HashMismatch {
                        expected: hash,
                        observed: hash_object(&bytes),
                        context: "sqlite-object-insert",
                    }
                });
            }
        }
        None => {
            tx.execute(
                "INSERT INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
                params![hash.as_bytes(), object.domain(), object.bytes()],
            )
            .map_err(|e| sqlite_err("insert object", e))?;
        }
    }
    Ok(())
}

/// The record row: insert-once with verify-on-retry against the
/// records table; the chain-wide idempotency-key uniqueness is
/// enforced by the partial unique index — its violation is the
/// `duplicate-idempotency-key` chain rule (nothing installed; retry
/// only with corrected input).
fn insert_record_once(
    tx: &rusqlite::Transaction<'_>,
    record: &RevisionRecord,
    record_bytes: &[u8],
    record_id: &Hash,
) -> Result<(), MediumError> {
    let existing: Option<Vec<u8>> = tx
        .query_row(
            "SELECT bytes FROM records WHERE hash = ?1",
            params![record_id.as_bytes()],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(no_rows_as_none)
        .map_err(|e| sqlite_err("read existing record", e))?;
    match existing {
        Some(bytes) => {
            // Content addressing makes a byte mismatch impossible for
            // a true retry; a mismatch is stored-row corruption.
            if bytes != record_bytes {
                return Err(MediumError::HashMismatch {
                    expected: *record_id,
                    observed: hash_object(&bytes),
                    context: "sqlite-record-insert",
                });
            }
        }
        None => {
            let kind = match record.kind {
                RecordKind::Founding { .. } => 0i64,
                RecordKind::Regular => 1i64,
            };
            let parent: Option<&[u8]> = record.parent.as_ref().map(|hash| &hash.as_bytes()[..]);
            let idem: Option<&[u8]> = record.idempotency_key.as_deref();
            tx.execute(
                "INSERT INTO records (hash, bytes, epoch, parent, idem, kind, tick) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    record_id.as_bytes(),
                    record_bytes,
                    record.epoch as i64,
                    parent,
                    idem,
                    kind,
                    record.tick as i64,
                ],
            )
            .map_err(|e| {
                if is_constraint(&e) {
                    MediumError::Chain {
                        rule: "duplicate-idempotency-key",
                    }
                } else {
                    sqlite_err("insert record", e)
                }
            })?;
        }
    }
    Ok(())
}

/// The rollback classification: a clean rollback propagates the typed
/// error (nothing installed); a failed rollback (including the
/// injected one) is `Unknown{RollbackUncertain}` — the disposition is
/// genuinely unknown.
fn rollback_or_unknown(
    tx: rusqlite::Transaction<'_>,
    fault: &Option<SqliteFaultControl>,
    error: MediumError,
) -> Result<AppendOutcome, MediumError> {
    if let Err(injected) = fault_check(fault, SqliteFaultPoint::AtRollback) {
        let _ = injected;
        return Ok(AppendOutcome::Unknown {
            reason: UnknownReason::RollbackUncertain,
        });
    }
    match tx.rollback() {
        Ok(()) => Err(error),
        Err(_) => Ok(AppendOutcome::Unknown {
            reason: UnknownReason::RollbackUncertain,
        }),
    }
}

// ------------------------------------------------------------- the reads

/// Reads and verifies the `meta.head` row: the empty blob is the
/// unfounded sentinel; exactly 32 bytes is the head record's
/// identity; anything else is fail-closed corruption.
fn read_meta_head(conn: &Connection) -> Result<Option<Hash>, MediumError> {
    let value: Vec<u8> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![META_HEAD],
            |row| row.get(0),
        )
        .map_err(|e| sqlite_err("read head row", e))?;
    match value.len() {
        0 => Ok(None),
        32 => Hash::from_bytes(&value)
            .map(Some)
            .ok_or_else(|| MediumError::CorruptHead {
                detail: "head hash width".to_owned(),
            }),
        _ => Err(MediumError::CorruptHead {
            detail: format!("head row has {} bytes (expected 0 or 32)", value.len()),
        }),
    }
}

/// Loads a record row's canonical bytes, hash-verified.
fn load_record_verified(conn: &Connection, id: &Hash) -> Result<Vec<u8>, MediumError> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT bytes FROM records WHERE hash = ?1",
            params![id.as_bytes()],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(no_rows_as_none)
        .map_err(|e| sqlite_err("read record", e))?;
    let Some(bytes) = bytes else {
        return Err(MediumError::MissingObject { hash: *id });
    };
    if hash_object(&bytes) != *id {
        return Err(MediumError::HashMismatch {
            expected: *id,
            observed: hash_object(&bytes),
            context: "sqlite-record-store",
        });
    }
    Ok(bytes)
}

/// Loads an object row's bytes, hash-verified (the plain mismatch
/// form; the walk's referenced form escalates to
/// `ReferencedObjectCorrupt`).
fn load_object_verified(conn: &Connection, hash: &Hash) -> Result<Vec<u8>, MediumError> {
    let bytes = load_object(conn, hash)?;
    if hash_object(&bytes) != *hash {
        return Err(MediumError::HashMismatch {
            expected: *hash,
            observed: hash_object(&bytes),
            context: "sqlite-object-store",
        });
    }
    Ok(bytes)
}

fn load_object(conn: &Connection, hash: &Hash) -> Result<Vec<u8>, MediumError> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT bytes FROM objects WHERE hash = ?1",
            params![hash.as_bytes()],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(no_rows_as_none)
        .map_err(|e| sqlite_err("read object", e))?;
    bytes.ok_or(MediumError::MissingObject { hash: *hash })
}

// ------------------------------------------------------------- the walks

/// The cold founding → HEAD walk (the complete validation): every
/// record row hash-verified and decoded, epoch-linked with checked
/// arithmetic, key-unique; every referenced object loaded and
/// hash-verified; every `depends_on` resolving to a strictly earlier
/// revision of the same authoritative history (retained, or
/// reclaim-indexed with its indexed epoch). Rebuilds the cursor.
///
/// **The compacted mode (PP-B3):** an absent parent ROW is a
/// legitimately-reclaimed revision only when the most recent on-chain
/// MK's skip table covers the gap — a skip for exactly the record
/// whose parent is absent (or that MK's own self entry), jumping to
/// the nearest retained ancestor under the gap-aware
/// strictly-earlier epoch linkage. An uncovered absence is hard
/// corruption — fail closed, never assumed reclamation.
fn walk_chain(conn: &Connection, head: Hash) -> Result<WalkedChain, MediumError> {
    walk_chain_excluding(conn, head, &BTreeSet::new())
}

/// The authenticated virtual-exclusion survivor walk (K2-C2B P2):
/// the cold walk with the AUDITED retiring-record set treated as
/// virtually absent — a physically present record in the exclusion
/// routes through the SAME authenticated skip jump an absent one
/// takes, and the MK-index staging automatically carries the
/// excluded records into the reclaimed-membership universe
/// (preserving depends_on resolution and key history exactly as
/// physical reclamation does). Every walked record and object
/// remains hash-verified; an unrelated missing parent still fails
/// closed.
fn walk_chain_excluding(
    conn: &Connection,
    head: Hash,
    exclude: &BTreeSet<Hash>,
) -> Result<WalkedChain, MediumError> {
    let mut seen_ids = BTreeSet::new();
    let mut seen_keys = BTreeSet::new();
    let mut reachable = BTreeSet::new();
    let mut records = 0u64;
    let mut content_objects = 0u64;
    let mut current = head;
    let mut newer_epoch: Option<u64> = None;
    let mut head_epoch = 0u64;
    // (id, epoch, depends_on) per record in walk order (newest
    // first) — dependency ancestry is validated once the chain is
    // complete.
    let mut walked: Vec<(Hash, u64, Vec<Hash>)> = Vec::new();
    // The staged per-MK index entries — merged into the universe
    // only after the walk completes (pre-reclamation the range
    // records are still present and retained; only genuinely absent
    // entries are reclaimed members).
    let mut staged_indexes: Vec<ReclaimedIndexEntry> = Vec::new();
    let mut checkpoints: Vec<Hash> = Vec::new();
    // The most recent MK's skip context — the gap cover for
    // everything below it — and the pending-gap flag the linkage
    // check consumes at the jump target.
    let mut skip_table: Option<(Hash, CheckpointMetadata)> = None;
    let mut gap_pending = false;
    let (founding_id, authority, campaign_id) = loop {
        if !seen_ids.insert(current) {
            return Err(MediumError::Chain { rule: "cycle" });
        }
        let bytes = load_referenced_record(conn, &current)?;
        let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
        if records == 0 {
            head_epoch = record.epoch;
        }
        // Checked linkage: an epoch of u64::MAX has no successor — it
        // must fail closed, never wrap. Across a skip-table jump the
        // linkage is gap-aware instead: the target must sit STRICTLY
        // below the pre-gap record (any gap size ≥ 1 — the reclaimed
        // span was removed whole).
        if let Some(epoch) = newer_epoch {
            if gap_pending {
                if record.epoch >= epoch {
                    return Err(MediumError::Chain {
                        rule: "epoch-regression",
                    });
                }
                gap_pending = false;
            } else if record.epoch.checked_add(1) != Some(epoch) {
                return Err(MediumError::Chain {
                    rule: "epoch-regression",
                });
            }
        }
        if let Some(key) = &record.idempotency_key
            && !seen_keys.insert(key.clone())
        {
            return Err(MediumError::Chain {
                rule: "duplicate-idempotency-key",
            });
        }
        let mut first_object: Option<Vec<u8>> = None;
        for (position, reference) in record.content_refs.iter().enumerate() {
            let bytes = load_referenced_object(conn, reference)?;
            if position == 0 {
                first_object = Some(bytes);
            }
            if reachable.insert(*reference) {
                content_objects += 1;
            }
        }
        // The MK recognition: a record whose first content object
        // carries the checkpoint-metadata domain — its index joins
        // the membership universe (staged for the post-walk merge)
        // and its skip table becomes the gap cover for everything
        // below it.
        if let Some(bytes) = first_object {
            let domain = peek_object_domain(&bytes).map_err(MediumError::Format)?;
            // The R9 corrective: the VERIFIED composite-closure
            // reachability (the same discipline as the file medium's
            // cold walk): a /2 root's chunks are transitively
            // referenced through its manifest — every dependency is
            // loaded hash-verified and fully envelope-checked before
            // it joins the reachable set. A missing, corrupt, or
            // malformed chunk fails the walk closed.
            if domain == composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN {
                let manifest = composite_manifest::decode(&bytes).map_err(MediumError::Format)?;
                for entry in &manifest.entries {
                    let chunk_bytes = load_referenced_object(conn, &entry.identity)?;
                    crate::composite_append::verify_chunk_dependency(
                        &chunk_bytes,
                        &entry.identity,
                        &manifest,
                    )
                    .map_err(|_error| MediumError::Chain {
                        rule: "composite-closure-verification",
                    })?;
                    if reachable.insert(entry.identity) {
                        content_objects += 1;
                    }
                }
            }
            if domain == CHECKPOINT_METADATA_DOMAIN {
                let metadata = CheckpointMetadata::decode(&bytes).map_err(MediumError::Format)?;
                staged_indexes.extend(metadata.index.iter().cloned());
                checkpoints.push(current);
                skip_table = Some((current, metadata));
            }
        }
        reachable.insert(current);
        walked.push((current, record.epoch, record.depends_on.clone()));
        records += 1;
        newer_epoch = Some(record.epoch);
        match record.parent {
            Some(parent) => {
                // The compacted walk: an absent parent row is a gap —
                // legitimate only under the covering skip table;
                // anything else is hard corruption (fail closed,
                // never assumed reclamation). The K2-C2B P2
                // exclusion: an AUDITED retiring record routes
                // through the same authenticated jump (the
                // directive's chain-verified membership is the only
                // virtual-absence authority).
                if record_exists(conn, &parent)? && !exclude.contains(&parent) {
                    current = parent;
                } else {
                    let Some((owner, table)) = skip_table.as_ref() else {
                        return Err(MediumError::MissingObject { hash: parent });
                    };
                    let target = if current == *owner {
                        table
                            .self_skip()
                            .map(|entry| entry.nearest_retained_ancestor)
                    } else {
                        table
                            .skip_for(&current)
                            .map(|entry| entry.nearest_retained_ancestor)
                    };
                    let Some(target) = target else {
                        return Err(MediumError::MissingObject { hash: parent });
                    };
                    gap_pending = true;
                    current = target;
                }
            }
            None => match record.kind {
                RecordKind::Founding {
                    authority,
                    campaign_id,
                } => {
                    if record.epoch != 0 {
                        return Err(MediumError::Chain {
                            rule: "kind-incoherent",
                        });
                    }
                    break (current, authority, campaign_id);
                }
                RecordKind::Regular => {
                    return Err(MediumError::Chain {
                        rule: "founding-missing",
                    });
                }
            },
        }
    };
    // The membership universe merge: only genuinely-absent entries
    // are reclaimed members — pre-reclamation the range records are
    // still present, retained, and already accounted (their index
    // entries are the evidence for the FUTURE reclamation, never
    // double-counted).
    let mut reclaimed: BTreeMap<Hash, u64> = BTreeMap::new();
    let mut reclaimed_records = 0u64;
    let mut reclaimed_keys = 0u64;
    for entry in staged_indexes {
        if seen_ids.contains(&entry.hash) {
            continue;
        }
        if reclaimed.insert(entry.hash, entry.epoch).is_some() {
            return Err(MediumError::Chain {
                rule: "checkpoint-index-incoherent",
            });
        }
        if let Some(key) = &entry.key
            && !seen_keys.insert(key.clone())
        {
            return Err(MediumError::Chain {
                rule: "duplicate-idempotency-key",
            });
        }
        if entry.key.is_some() {
            reclaimed_keys += 1;
        }
        reclaimed_records += 1;
    }
    // Dependency ancestry, deferred until the chain is complete: the
    // walk runs newest-first, so a record at position i can only
    // depend on revisions at positions > i (strictly earlier), or on
    // a reclaim-indexed revision with its indexed epoch strictly
    // below the referencing record's.
    let mut positions: BTreeMap<Hash, usize> = BTreeMap::new();
    for (index, (id, _, _)) in walked.iter().enumerate() {
        positions.insert(*id, index);
    }
    for (index, (_, record_epoch, dependencies)) in walked.iter().enumerate() {
        for dependency in dependencies {
            let resolves = positions
                .get(dependency)
                .is_some_and(|position| *position > index)
                || reclaimed
                    .get(dependency)
                    .is_some_and(|&dep_epoch| dep_epoch < *record_epoch);
            if !resolves {
                return Err(MediumError::Chain {
                    rule: "depends-on-unresolvable",
                });
            }
        }
    }
    Ok(WalkedChain {
        summary: ChainSummary {
            head,
            height: head_epoch,
            founding: founding_id,
            authority,
            campaign_id: campaign_id.clone(),
            records,
            content_objects,
            checkpoints,
            reclaimed_records,
            reclaimed_keys,
        },
        cursor: SqliteCursor {
            head,
            epoch: head_epoch,
            seen_keys,
            reachable,
            record_ids: seen_ids,
            reclaimed,
            founding: founding_id,
            authority,
            campaign_id,
            content_objects,
        },
    })
}

/// The walk's referenced record load — escalates a hash mismatch to
/// `ReferencedObjectCorrupt` (fail closed; the row is never replaced).
fn load_referenced_record(conn: &Connection, id: &Hash) -> Result<Vec<u8>, MediumError> {
    load_record_verified(conn, id).map_err(|error| match error {
        MediumError::HashMismatch { .. } => MediumError::ReferencedObjectCorrupt { hash: *id },
        other => other,
    })
}

fn load_referenced_object(conn: &Connection, hash: &Hash) -> Result<Vec<u8>, MediumError> {
    load_object_verified(conn, hash).map_err(|error| match error {
        MediumError::HashMismatch { .. } => MediumError::ReferencedObjectCorrupt { hash: *hash },
        other => other,
    })
}

/// Whether a record row exists (the compacted walk's gap detection —
/// a legitimately-reclaimed parent is an absent row, covered by the
/// covering MK's skip table; the binding is the raw identity bytes,
/// the table's established key form).
fn record_exists(conn: &Connection, id: &Hash) -> Result<bool, MediumError> {
    match conn.query_row(
        "SELECT 1 FROM records WHERE hash = ?1",
        params![id.as_bytes()],
        |_| Ok(()),
    ) {
        Ok(()) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(error) => Err(sqlite_err("record exists", error)),
    }
}

/// Validates the observed head against the trusted cursor: O(0)
/// record walks on the verified head, a bounded suffix walk on
/// external divergence, a cold walk with no cursor, and fail-closed
/// divergence when the head vanished or is not a descendant.
fn validate_observed(
    conn: &Connection,
    cursor: &mut Option<SqliteCursor>,
    walks: &mut WalkStats,
    observed: Option<Hash>,
) -> Result<(), MediumError> {
    let trusted = cursor.as_ref().map(|cursor| (cursor.head, cursor.epoch));
    match (observed, trusted) {
        (None, None) => Ok(()),
        (Some(id), Some((cursor_head, _))) if id == cursor_head => {
            walks.last_append_walked = 0;
            Ok(())
        }
        (Some(id), Some((trusted_head, trusted_epoch))) => {
            let (trusted_keys, trusted_record_ids, reclaimed) = cursor
                .as_ref()
                .map(|cursor| {
                    (
                        cursor.seen_keys.clone(),
                        cursor.record_ids.clone(),
                        cursor.reclaimed.clone(),
                    )
                })
                .unwrap_or_default();
            match walk_suffix(
                conn,
                id,
                trusted_head,
                trusted_epoch,
                trusted_keys,
                trusted_record_ids,
                reclaimed,
            ) {
                Ok(suffix) => {
                    walks.last_append_walked = suffix.len() as u64;
                    // R15: the entries already carry their
                    // walk-verified composite closures — no
                    // re-extraction (no duplicate hash-verifying
                    // reads) and no verification failure converted
                    // into an absent closure: absorption consumes
                    // only verified entries.
                    absorb_suffix(cursor, suffix, id);
                    Ok(())
                }
                Err(MediumError::Chain { rule: "divergence" }) => {
                    Err(MediumError::Chain { rule: "divergence" })
                }
                Err(_) => {
                    // Sound invalidation: discard the cursor; the full
                    // walk rebuilds it or fails closed.
                    *cursor = None;
                    match walk_chain(conn, id) {
                        Ok(walked) => {
                            walks.last_append_walked = walked.summary.records;
                            *cursor = Some(walked.cursor);
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                }
            }
        }
        (Some(id), None) => {
            let walked = walk_chain(conn, id)?;
            walks.last_append_walked = walked.summary.records;
            *cursor = Some(walked.cursor);
            Ok(())
        }
        (None, Some(_)) => Err(MediumError::Chain { rule: "divergence" }),
    }
}

/// The VERIFIED composite closure over the SQLite connection (the
/// R12 corrective): for a /2 root, strictly decode the manifest and
/// verify every declared chunk (the hash-verified load + the full
/// envelope check) — returning the closure identities for the
/// trusted reachability set. Every failure propagates typed;
/// `Ok(None)` is ONLY the positively established non-composite case
/// (the peeked domain is not the /2 root) — never a suppressed
/// verification failure.
fn verified_composite_closure(
    conn: &Connection,
    root_bytes: &[u8],
) -> Result<Option<Vec<Hash>>, MediumError> {
    let domain = peek_object_domain(root_bytes).map_err(MediumError::Format)?;
    if domain != composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN {
        return Ok(None);
    }
    let manifest = composite_manifest::decode(root_bytes).map_err(MediumError::Format)?;
    let mut identities = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        let chunk_bytes = load_referenced_object(conn, &entry.identity)?;
        crate::composite_append::verify_chunk_dependency(&chunk_bytes, &entry.identity, &manifest)
            .map_err(|_error| MediumError::Chain {
                rule: "composite-closure-verification",
            })?;
        identities.push(entry.identity);
    }
    Ok(Some(identities))
}

/// One suffix revision with its walk-verified composite closure (the
/// R15 corrective): the closure was verified DURING the suffix walk
/// by the established closure verifier over the record's FIRST
/// content reference — exactly the cold walk's discipline — so
/// `None` is positively established non-composite content (the
/// peeked domain is not the /2 root), NEVER a suppressed
/// verification failure: every required failure propagated typed
/// before this entry could exist. The entries are produced only by
/// [`walk_suffix`] and consumed only by [`absorb_suffix`] — no other
/// path extracts a closure.
struct VerifiedSuffixEntry {
    id: Hash,
    record: RevisionRecord,
    composite_closure: Option<Vec<Hash>>,
}

/// Walks `target` down its parents until `until` (exclusive),
/// verifying each record and its content references — the same
/// invariants the cold walk enforces, including the trusted-cursor
/// boundary epoch and dependency ancestry over the trusted record
/// set, the strictly-older suffix positions, and the reclaim-indexed
/// revisions' indexed epochs. A suffix may pass a foreign MK — its
/// record and metadata object validate like any other, but its index
/// is not absorbed into this handle's cursor (the universe is rebuilt
/// by the next cold walk; the single-writer discipline makes a
/// mid-handle foreign MK one this process published through another,
/// already-closed handle). R15: each returned entry carries its
/// walk-verified composite closure (the established verifier runs
/// during the walk — see [`VerifiedSuffixEntry`]).
fn walk_suffix(
    conn: &Connection,
    target: Hash,
    until: Hash,
    trusted_epoch: u64,
    trusted_keys: BTreeSet<Vec<u8>>,
    trusted_record_ids: BTreeSet<Hash>,
    reclaimed: BTreeMap<Hash, u64>,
) -> Result<Vec<VerifiedSuffixEntry>, MediumError> {
    let mut suffix: Vec<VerifiedSuffixEntry> = Vec::new();
    let mut current = target;
    let mut newer_epoch: Option<u64> = None;
    let mut visited = BTreeSet::new();
    let mut suffix_keys = BTreeSet::new();
    loop {
        if current == until {
            if let Some(boundary) = suffix.last()
                && trusted_epoch.checked_add(1) != Some(boundary.record.epoch)
            {
                return Err(MediumError::Chain {
                    rule: "epoch-regression",
                });
            }
            let mut positions: BTreeMap<Hash, usize> = BTreeMap::new();
            for (index, entry) in suffix.iter().enumerate() {
                positions.insert(entry.id, index);
            }
            for (index, entry) in suffix.iter().enumerate() {
                for dependency in &entry.record.depends_on {
                    let resolves = trusted_record_ids.contains(dependency)
                        || positions
                            .get(dependency)
                            .is_some_and(|position| *position > index)
                        || reclaimed
                            .get(dependency)
                            .is_some_and(|&dep_epoch| dep_epoch < entry.record.epoch);
                    if !resolves {
                        return Err(MediumError::Chain {
                            rule: "depends-on-unresolvable",
                        });
                    }
                }
            }
            return Ok(suffix);
        }
        if !visited.insert(current) {
            return Err(MediumError::Chain { rule: "cycle" });
        }
        let bytes = load_referenced_record(conn, &current)?;
        let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
        if let Some(epoch) = newer_epoch
            && record.epoch.checked_add(1) != Some(epoch)
        {
            return Err(MediumError::Chain {
                rule: "epoch-regression",
            });
        }
        if let Some(key) = &record.idempotency_key
            && (trusted_keys.contains(key) || !suffix_keys.insert(key.clone()))
        {
            return Err(MediumError::Chain {
                rule: "duplicate-idempotency-key",
            });
        }
        // R15: every content reference loads hash-verified; the
        // FIRST reference's domain determines the entry's composite
        // closure — the cold walk's exact discipline — and the
        // established verifier runs HERE, during the walk: a /2
        // root's full closure is verified exactly once (no
        // re-extraction, no duplicate hash-verifying reads), a
        // malformed root domain refuses (the peek propagates), and a
        // missing, hash-mismatched, or malformed chunk refuses — a
        // failure is never converted into an absent closure.
        let mut first_object: Option<Vec<u8>> = None;
        for (position, reference) in record.content_refs.iter().enumerate() {
            let bytes = load_referenced_object(conn, reference)?;
            if position == 0 {
                first_object = Some(bytes);
            }
        }
        let composite_closure = match first_object {
            Some(bytes) => verified_composite_closure(conn, &bytes)?,
            None => None,
        };
        newer_epoch = Some(record.epoch);
        let parent = record.parent;
        suffix.push(VerifiedSuffixEntry {
            id: current,
            record,
            composite_closure,
        });
        match parent {
            Some(parent) => current = parent,
            None => return Err(MediumError::Chain { rule: "divergence" }),
        }
    }
}

/// Merges a verified suffix (newest-first) into the cursor and points
/// it at the new head — oldest-first absorption so the final epoch is
/// the newest verified one. R15: the entries carry their
/// walk-verified composite closures (verified during the walk, never
/// re-extracted here) — the cursor absorbs only verified entries,
/// after the whole suffix passed every check.
fn absorb_suffix(
    cursor: &mut Option<SqliteCursor>,
    suffix: Vec<VerifiedSuffixEntry>,
    new_head: Hash,
) {
    let Some(cursor) = cursor.as_mut() else {
        return;
    };
    for entry in suffix.into_iter().rev() {
        let VerifiedSuffixEntry {
            id,
            record,
            composite_closure,
        } = entry;
        if let Some(key) = &record.idempotency_key {
            cursor.seen_keys.insert(key.clone());
        }
        cursor.reachable.insert(id);
        cursor.record_ids.insert(id);
        for reference in &record.content_refs {
            if cursor.reachable.insert(*reference) {
                cursor.content_objects += 1;
            }
        }
        // The walk-verified composite closure joins the trusted
        // reachability set — the same identities the cold walk
        // collects over the same chain (`None` is positively
        // established non-composite content).
        if let Some(closure) = composite_closure {
            for identity in closure {
                if cursor.reachable.insert(identity) {
                    cursor.content_objects += 1;
                }
            }
        }
        cursor.epoch = record.epoch;
    }
    cursor.head = new_head;
}

/// Advances the cursor over an accepted append — O(1) work (plus
/// the verified composite closure when the record is a /2 root —
/// the R12 corrective keeps the hot cursor on the same definition
/// of reachability as the cold walk).
fn advance_cursor(
    cursor: &mut Option<SqliteCursor>,
    record: &RevisionRecord,
    id: &Hash,
    composite_closure: Option<&[Hash]>,
) {
    let refs = &record.content_refs;
    match cursor.as_mut() {
        None => {
            let RecordKind::Founding {
                authority,
                campaign_id,
            } = &record.kind
            else {
                return;
            };
            let mut seen_keys = BTreeSet::new();
            if let Some(key) = &record.idempotency_key {
                seen_keys.insert(key.clone());
            }
            let mut reachable = BTreeSet::new();
            reachable.insert(*id);
            let mut record_ids = BTreeSet::new();
            record_ids.insert(*id);
            let mut content_objects = 0u64;
            for reference in refs {
                if reachable.insert(*reference) {
                    content_objects += 1;
                }
            }
            if let Some(closure) = composite_closure {
                for identity in closure {
                    if reachable.insert(*identity) {
                        content_objects += 1;
                    }
                }
            }
            *cursor = Some(SqliteCursor {
                head: *id,
                epoch: record.epoch,
                seen_keys,
                reachable,
                record_ids,
                reclaimed: BTreeMap::new(),
                founding: *id,
                authority: *authority,
                campaign_id: campaign_id.clone(),
                content_objects,
            });
        }
        Some(cursor) => {
            cursor.head = *id;
            cursor.epoch = record.epoch;
            if let Some(key) = &record.idempotency_key {
                cursor.seen_keys.insert(key.clone());
            }
            cursor.reachable.insert(*id);
            cursor.record_ids.insert(*id);
            for reference in refs {
                if cursor.reachable.insert(*reference) {
                    cursor.content_objects += 1;
                }
            }
            if let Some(closure) = composite_closure {
                for identity in closure {
                    if cursor.reachable.insert(*identity) {
                        cursor.content_objects += 1;
                    }
                }
            }
        }
    }
}

// --------------------------------------------------------- the meta rows

/// The §6.1 provenance row's value: the sealed legacy chain's head
/// (the binding the activation writes once; the row's key carries
/// its meaning, the authority-row precedent).
fn provenance_bytes(source_head: &Hash) -> Vec<u8> {
    source_head.as_bytes().to_vec()
}

/// Reads a meta row by key (`None` when absent).
fn read_meta_row(conn: &Connection, key: &str) -> Result<Option<Vec<u8>>, MediumError> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .map(Some)
    .or_else(no_rows_as_none)
    .map_err(|e| sqlite_err("read meta row", e))
}

/// The migration-pending marker's wire domain (§6.1's target
/// lifecycle — the review corrective).
const MIGRATION_PENDING_DOMAIN: &str = "sektor7/engine-persist/migration-pending/1";
const MIGRATION_PENDING_VERSION: u32 = 1;

/// The pending marker's canonical bytes: the domain and its version.
fn migration_pending_bytes() -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(MIGRATION_PENDING_DOMAIN);
    enc.write_u32(MIGRATION_PENDING_VERSION);
    enc.into_bytes()
}

/// Decodes and verifies the pending marker (a corrupt marker is
/// fail-closed `CorruptHead` — the lifecycle evidence is never
/// guessed).
fn decode_migration_pending(bytes: &[u8]) -> Result<(), MediumError> {
    let corrupt = |detail: String| MediumError::CorruptHead { detail };
    let mut dec = Decoder::new(bytes);
    dec.read_domain(MIGRATION_PENDING_DOMAIN)
        .map_err(|e| corrupt(format!("the migration-pending row: {e}")))?;
    let version = dec
        .read_u32()
        .map_err(|e| corrupt(format!("the migration-pending row: {e}")))?;
    if version != MIGRATION_PENDING_VERSION {
        return Err(corrupt(format!(
            "unsupported migration-pending version {version}"
        )));
    }
    dec.finish()
        .map_err(|e| corrupt(format!("the migration-pending row: {e}")))?;
    Ok(())
}

/// The authority row's encoding: `[32-byte founding hash][1-byte
/// authority mode][campaign id UTF-8]`.
fn authority_bytes(founding: &Hash, authority: AuthorityMode, campaign_id: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(Hash::WIDTH as usize + 1 + campaign_id.len());
    bytes.extend_from_slice(founding.as_bytes());
    bytes.push(match authority {
        AuthorityMode::Standalone => 0u8,
        AuthorityMode::Shared => 1u8,
    });
    bytes.extend_from_slice(campaign_id.as_bytes());
    bytes
}

/// The open-time cross-check: a founded chain must carry the matching
/// authority row; an unfounded store must not carry one. A
/// contradiction is fail-closed (the metadata was tampered with).
fn verify_authority_row(
    conn: &Connection,
    cursor: Option<&SqliteCursor>,
) -> Result<(), MediumError> {
    let present: Option<Vec<u8>> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![META_AUTHORITY],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(no_rows_as_none)
        .map_err(|e| sqlite_err("read authority row", e))?;
    match (cursor, present) {
        (None, None) => Ok(()),
        (Some(cursor), Some(row)) => {
            let expected = authority_bytes(&cursor.founding, cursor.authority, &cursor.campaign_id);
            if row == expected {
                Ok(())
            } else {
                Err(MediumError::CorruptHead {
                    detail: "the authority row contradicts the validated chain".to_owned(),
                })
            }
        }
        (Some(_), None) => Err(MediumError::CorruptHead {
            detail: "a founded chain is missing the authority row".to_owned(),
        }),
        (None, Some(_)) => Err(MediumError::CorruptHead {
            detail: "an unfounded store carries an authority row".to_owned(),
        }),
    }
}

// ------------------------------------------------------------- utilities

/// Configures the writable connection: WAL (set and verified — the
/// statement returns the resulting mode), FULL (set as a pure setter
/// — the statement returns no row — then **verified by reading the
/// actual configuration back**, the numeric form 2), and the busy
/// timeout. A request is never assumed granted.
fn configure_writable(conn: &Connection, config: &SqliteMediumConfig) -> Result<(), MediumError> {
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(|e| sqlite_err("set journal_mode", e))?;
    if mode != "wal" {
        return Err(MediumError::Sqlite {
            op: "verify journal_mode",
            detail: format!("expected wal, observed {mode}"),
        });
    }
    conn.execute_batch("PRAGMA synchronous=FULL")
        .map_err(|e| sqlite_err("set synchronous", e))?;
    let sync: i64 = conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(|e| sqlite_err("verify synchronous", e))?;
    if sync != 2 {
        return Err(MediumError::Sqlite {
            op: "verify synchronous",
            detail: format!("expected 2 (FULL), observed {sync}"),
        });
    }
    conn.busy_timeout(config.busy_timeout)
        .map_err(|e| sqlite_err("set busy_timeout", e))?;
    Ok(())
}

fn fault_check(
    fault: &Option<SqliteFaultControl>,
    point: SqliteFaultPoint,
) -> Result<(), MediumError> {
    match fault {
        Some(control) => control.check(point),
        None => Ok(()),
    }
}

fn sqlite_err(op: &'static str, error: rusqlite::Error) -> MediumError {
    MediumError::Sqlite {
        op,
        detail: error.to_string(),
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::DatabaseBusy,
                ..
            },
            _
        )
    )
}

/// The open-path write statements (the schema batch, the unfounded
/// head-row seed) take the same SQLite write lock the append's
/// `BEGIN IMMEDIATE` acquires: a busy-timeout expiry under a
/// concurrent writer is the SAME ratified contention condition —
/// the typed abort, never the generic surface. Every other error
/// keeps the generic `Sqlite` mapping (never a blanket acceptance;
/// the discrimination is exactly `is_busy`).
fn sqlite_err_or_contention(op: &'static str, error: rusqlite::Error) -> MediumError {
    if is_busy(&error) {
        MediumError::Contention {
            detail: format!(
                "the write lock was unavailable within the busy timeout \
                 while {op} — a concurrent writer holds the database"
            ),
        }
    } else {
        sqlite_err(op, error)
    }
}

fn is_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::ConstraintViolation,
                ..
            },
            _
        )
    )
}

/// Maps the query-result no-rows case to `None` (the absent-row
/// reads); every other error propagates.
fn no_rows_as_none<T>(error: rusqlite::Error) -> Result<Option<T>, rusqlite::Error> {
    match error {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b1-{}-{}",
            name,
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

    fn sample_object(payload: &[u8]) -> StoredObject {
        // A well-formed domain-tagged object (the Encoder's
        // length-prefixed string form).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(7u64).to_le_bytes());
        bytes.extend_from_slice(b"s7/test/");
        bytes.extend_from_slice(payload);
        StoredObject::new(bytes).unwrap()
    }

    #[test]
    fn fresh_open_is_unfounded_and_founds_cleanly() {
        let path = temp_db("fresh");
        let mut medium = SqliteMedium::open(&path).unwrap();
        assert_eq!(medium.head().unwrap(), None);
        assert!(medium.campaign().is_none());

        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "pp-b1-test", 0);
        let outcome = medium.append(&founding, &[], None).unwrap();
        let AppendOutcome::Accepted { tier, .. } = outcome else {
            panic!("the founding append accepts: {outcome:?}")
        };
        assert_eq!(tier, SyncTier::TransactionalWalFull);
        assert_eq!(medium.head().unwrap(), Some(founding.id()));
        assert_eq!(medium.campaign().map(|(f, _, _)| f), Some(founding.id()));

        let summary = medium.validate().unwrap();
        assert_eq!(summary.head, founding.id());
        assert_eq!(summary.height, 0);
        assert_eq!(summary.records, 1);
        drop(medium);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reopen_validates_and_the_authority_row_survives() {
        let path = temp_db("reopen");
        {
            let mut medium = SqliteMedium::open(&path).unwrap();
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, "pp-b1-reopen", 3);
            medium.append(&founding, &[], None).unwrap();
        }
        {
            let mut medium = SqliteMedium::open(&path).unwrap();
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, "pp-b1-reopen", 3);
            assert_eq!(medium.head().unwrap(), Some(founding.id()));
            let summary = medium.validate().unwrap();
            assert_eq!(summary.campaign_id, "pp-b1-reopen");
            assert_eq!(summary.records, 1);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn regular_append_walks_and_rejects_stale_parents() {
        let path = temp_db("regular");
        let mut medium = SqliteMedium::open(&path).unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "pp-b1-regular", 0);
        medium.append(&founding, &[], None).unwrap();
        let founding_id = founding.id();

        let object = sample_object(b"payload-one");
        let regular =
            RevisionRecord::regular(founding_id, 0, 9).with_content_refs(vec![object.hash()]);
        let AppendOutcome::Accepted { id, .. } = medium
            .append(&regular, &[object], Some(founding_id))
            .unwrap()
        else {
            panic!("the regular append accepts")
        };
        assert_eq!(id, regular.id());

        // A stale writer against the superseded parent.
        let stale_object = sample_object(b"payload-two");
        let stale = RevisionRecord::regular(founding_id, 0, 10)
            .with_content_refs(vec![stale_object.hash()]);
        let outcome = medium
            .append(&stale, &[stale_object], Some(founding_id))
            .unwrap();
        assert_eq!(
            outcome,
            AppendOutcome::RejectedStaleParent {
                observed: Some(regular.id())
            }
        );

        let summary = medium.validate().unwrap();
        assert_eq!(summary.records, 2);
        assert_eq!(summary.height, 1);
        drop(medium);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_only_audit_validates_and_refuses_appends() {
        let path = temp_db("readonly");
        {
            let mut medium = SqliteMedium::open(&path).unwrap();
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, "pp-b1-audit", 0);
            medium.append(&founding, &[], None).unwrap();
        }
        let mut audit = SqliteMedium::open_read_only(&path).unwrap();
        let summary = audit.validate().unwrap();
        assert_eq!(summary.records, 1);
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "pp-b1-audit", 0);
        let refused = audit.append(&founding, &[], None);
        assert!(matches!(refused, Err(MediumError::Sqlite { .. })));
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod exclusion_tests {
    use super::*;
    use crate::checkpoint::{CheckpointMetadata, ReclaimedIndexEntry, SkipEntry};
    use crate::medium::CanonicalMedium;
    use crate::record::{AuthorityMode, RevisionRecord};

    #[test]
    fn a_successful_exclusion_walk_over_a_real_checkpoint_range_sqlite() {
        // The SQLite positive-exclusion regression (the review's §5
        // requirement — the second provider's agreement with the
        // local result): a REAL hand-crafted checkpoint with the
        // authenticated self-skip; a successful nonempty
        // simulated-retirement walk over the audited range; the
        // survivor and reclaimed sets verified; the physical rows
        // untouched; the ordinary state intact.
        let root = std::env::temp_dir().join(format!(
            "s7-p2sqlex-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        let object = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let mut chain: Vec<Hash> = Vec::new();
        let mut epochs: Vec<u64> = Vec::new();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2sq", 0)
            .with_content_refs(vec![object.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&object), None)
            .unwrap();
        chain.push(founding.id());
        epochs.push(0);
        for tick in 1..=3u64 {
            let record = RevisionRecord::regular(*chain.last().unwrap(), tick - 1, tick)
                .with_content_refs(vec![object.hash()]);
            medium
                .append(
                    &record,
                    std::slice::from_ref(&object),
                    Some(*chain.last().unwrap()),
                )
                .unwrap();
            chain.push(record.id());
            epochs.push(tick);
        }
        let [_, p1, p2, sk] = [chain[0], chain[1], chain[2], chain[3]];
        let metadata = CheckpointMetadata::new(
            sk,
            p2,
            vec![SkipEntry::new(Hash::ZERO, p1)],
            vec![
                ReclaimedIndexEntry::new(None, p2, epochs[2]),
                ReclaimedIndexEntry::new(None, sk, epochs[3]),
            ],
            [7; 32],
            [9; 32],
        );
        let meta_object = StoredObject::new(metadata.canonical_bytes()).unwrap();
        let mk_record =
            RevisionRecord::regular(sk, epochs[3], 5).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk_record, std::slice::from_ref(&meta_object), Some(sk))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        assert_eq!(head, mk_record.id());

        let excluded: BTreeSet<Hash> = [p2, sk].into_iter().collect();
        let walked = walk_chain_excluding(&medium.conn, head, &excluded).unwrap();
        assert_eq!(
            walked.summary.records, 3,
            "MK, P1, F walk — the local agreement"
        );
        assert_eq!(walked.summary.reclaimed_records, 2);
        assert_eq!(walked.cursor.reclaimed.get(&p2), Some(&epochs[2]));
        assert_eq!(walked.cursor.reclaimed.get(&sk), Some(&epochs[3]));
        assert!(!walked.cursor.record_ids.contains(&p2));
        assert!(!walked.cursor.record_ids.contains(&sk));
        assert!(walked.cursor.record_ids.contains(&mk_record.id()));

        // The physical rows are untouched; the authoritative state
        // observes the ACTUAL chain.
        let authoritative = medium.validate().unwrap();
        assert_eq!(authoritative.records, 5);
        let summary = medium.head().unwrap().unwrap();
        assert_eq!(summary, mk_record.id());
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_verifier_crosses_a_reclaimed_gap_on_sqlite() {
        // (B1/B2 on the second provider) The genuine gap crossing
        // over SQLite — the target audit and MK BELOW a reclaimed
        // range; the descent crosses the authenticated skip; the
        // A4 manifest check exercises read_record (the records
        // table, the cross-provider interface).
        let root = std::env::temp_dir().join(format!(
            "s7-p2b2sq-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        let object = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let step = |medium: &mut SqliteMedium, tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record =
                RevisionRecord::regular(head, epoch, tick).with_content_refs(vec![object.hash()]);
            medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap();
            record.id()
        };
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2b2s", 0)
            .with_content_refs(vec![object.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&object), None)
            .unwrap();
        let _p1 = step(&mut medium, 1);
        let r1 = step(&mut medium, 2);
        let r1_epoch = medium.read_record(&r1).unwrap().epoch;
        let old_meta = CheckpointMetadata::new(
            r1,
            r1,
            Vec::new(),
            vec![ReclaimedIndexEntry::new(None, r1, r1_epoch)],
            [1; 32],
            [2; 32],
        );
        let old_object = StoredObject::new(old_meta.canonical_bytes()).unwrap();
        let mk_o =
            RevisionRecord::regular(r1, r1_epoch, 9).with_content_refs(vec![old_object.hash()]);
        medium
            .append(&mk_o, std::slice::from_ref(&old_object), Some(r1))
            .unwrap();
        let ledger = crate::reclaim::ReclamationLedger {
            mk: mk_o.id(),
            sk: r1,
            range_digest: old_meta.range_digest,
            seed: old_meta.seed,
            records: 1,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![crate::reclaim::ReclaimUnit {
                record: r1,
                objects: vec![object.hash()],
            }],
        };
        let (audit_o, outcome) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger, 10).unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        let p2n = step(&mut medium, 11);
        let p2n_epoch = medium.read_record(&p2n).unwrap().epoch;
        let sk_n = step(&mut medium, 12);
        let sk_n_epoch = medium.read_record(&sk_n).unwrap().epoch;
        let new_meta = CheckpointMetadata::new(
            sk_n,
            p2n,
            vec![SkipEntry::new(Hash::ZERO, audit_o)],
            vec![
                ReclaimedIndexEntry::new(None, p2n, p2n_epoch),
                ReclaimedIndexEntry::new(None, sk_n, sk_n_epoch),
            ],
            [3; 32],
            [4; 32],
        );
        let new_object = StoredObject::new(new_meta.canonical_bytes()).unwrap();
        let mk_new = RevisionRecord::regular(sk_n, sk_n_epoch, 13)
            .with_content_refs(vec![new_object.hash()]);
        medium
            .append(&mk_new, std::slice::from_ref(&new_object), Some(sk_n))
            .unwrap();
        // THE RECLAIMED GAP: delete the NEW range's rows from BOTH
        // tables (the provider's own retirement shape).
        for id in [p2n, sk_n] {
            medium
                .conn
                .execute(
                    "DELETE FROM records WHERE hash = ?1",
                    params![id.as_bytes()],
                )
                .unwrap();
            medium
                .conn
                .execute(
                    "DELETE FROM objects WHERE hash = ?1",
                    params![id.as_bytes()],
                )
                .unwrap();
        }
        let (verified, metadata) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit_o, &mk_o.id()).unwrap();
        assert_eq!(verified.mk, mk_o.id());
        assert_eq!(metadata.sk, r1);
        assert_eq!(verified.units.len(), 1);
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_first_installed_wrong_manifest_refuses_on_sqlite() {
        // (B3 on the second provider) The FIRST installed audit with
        // an omitted or injected object manifest refuses at the
        // exact comparison — the records-table read path.
        for wrong_objects in [
            Vec::new(),
            vec![crate::hash::hash_object(b"p2c3s-injected")],
        ] {
            let root = std::env::temp_dir().join(format!(
                "s7-p2c3s-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let mut medium = SqliteMedium::open(root.join("db.sqlite")).unwrap();
            let object = StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"payload");
                enc.into_bytes()
            })
            .unwrap();
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2c3s", 0)
                .with_content_refs(vec![object.hash()]);
            medium
                .append(&founding, std::slice::from_ref(&object), None)
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let r1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![object.hash()]);
            medium
                .append(&r1, std::slice::from_ref(&object), Some(head))
                .unwrap();
            let meta = CheckpointMetadata::new(
                r1.id(),
                r1.id(),
                Vec::new(),
                vec![ReclaimedIndexEntry::new(None, r1.id(), 1)],
                [1; 32],
                [2; 32],
            );
            let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
            let mk =
                RevisionRecord::regular(r1.id(), 1, 2).with_content_refs(vec![meta_object.hash()]);
            medium
                .append(&mk, std::slice::from_ref(&meta_object), Some(r1.id()))
                .unwrap();
            let ledger = crate::reclaim::ReclamationLedger {
                mk: mk.id(),
                sk: r1.id(),
                range_digest: meta.range_digest,
                seed: meta.seed,
                records: 1,
                reclaimed_keys: 0,
                journal_files: Vec::new(),
                units: vec![crate::reclaim::ReclaimUnit {
                    record: r1.id(),
                    objects: wrong_objects,
                }],
            };
            let (audit, outcome) =
                crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger, 3).unwrap();
            assert!(matches!(
                outcome,
                crate::medium::AppendOutcome::Accepted { .. }
            ));
            let error = crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk.id())
                .expect_err("the wrong manifest refuses on SQLite");
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-complete/units-objects-mismatch"
                    }
                ),
                "the exact error class: {error:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
    #[test]
    fn the_trusted_directive_executes_on_sqlite() {
        // The trusted execution on the second provider: the
        // successful authenticated directive under BEGIN IMMEDIATE —
        // the retiring records and the private object deleted from
        // both tables; the SHARED object (referenced by the
        // surviving root) SURVIVES; the deferred classification
        // reported; the pending probe complete; the post-retirement
        // validation works.
        let root = std::env::temp_dir().join(format!(
            "s7-p2txs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        let shared = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"shared-payload");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"private-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2txs", 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let step = |medium: &mut SqliteMedium, objects: &[StoredObject]| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
            let record = RevisionRecord::regular(head, epoch, refs.len() as u64 + 10)
                .with_content_refs(refs);
            medium.append(&record, objects, Some(head)).unwrap();
            record.id()
        };
        let _p1 = step(&mut medium, std::slice::from_ref(&shared));
        let r1 = step(&mut medium, &[shared.clone(), private.clone()]);
        let sk = step(&mut medium, std::slice::from_ref(&shared));
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, _p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, sk, 3),
            ],
            [1; 32],
            [2; 32],
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let mk = RevisionRecord::regular(sk, 3, 14).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(sk))
            .unwrap();
        let r1_refs = medium.read_record(&r1).unwrap().content_refs.clone();
        let sk_refs = medium.read_record(&sk).unwrap().content_refs.clone();
        let ledger = crate::reclaim::ReclamationLedger {
            mk: mk.id(),
            sk,
            range_digest: meta.range_digest,
            seed: meta.seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                crate::reclaim::ReclaimUnit {
                    record: r1,
                    objects: r1_refs,
                },
                crate::reclaim::ReclaimUnit {
                    record: sk,
                    objects: sk_refs,
                },
            ],
        };
        let (audit, outcome) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        let outcome = medium.reclaim(&directive).unwrap();
        assert_eq!(outcome.deferred, vec![shared.hash()]);
        assert!(!outcome.mandatory.contains(&shared.hash()));
        // The private object and both retiring records are gone from
        // BOTH tables; the shared object survives.
        let conn = &medium.conn;
        for id in [private.hash(), r1, sk] {
            let objects: i64 = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM objects WHERE hash = ?1)",
                    params![id.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap();
            let records: i64 = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM records WHERE hash = ?1)",
                    params![id.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(objects + records, 0, "the mandatory identity is gone");
        }
        let shared_present: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE hash = ?1)",
                params![shared.hash().as_bytes()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(shared_present, 1, "the shared object survives");
        assert!(!medium.reclaim_pending(&directive).unwrap());
        medium.validate().unwrap();
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
}
