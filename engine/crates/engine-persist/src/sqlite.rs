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
use crate::hash::{Hash, hash_object};
use crate::local::WalkStats;
use crate::medium::{
    AppendOutcome, CanonicalMedium, ChainSummary, MediumError, Reconciliation, StoredObject,
    SyncTier, UnknownReason,
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
            .map_err(|e| sqlite_err("create schema", e))?;
        // The unfounded sentinel: an empty head row, seeded once.
        conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('head', x'')",
            [],
        )
        .map_err(|e| sqlite_err("seed head row", e))?;
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
            .map_err(|e| sqlite_err("create schema", e))?;
        conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('head', x'')",
            [],
        )
        .map_err(|e| sqlite_err("seed head row", e))?;
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

    fn append(
        &mut self,
        record: &RevisionRecord,
        objects: &[StoredObject],
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError> {
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
        if reparsed != *record {
            return Err(MediumError::Chain {
                rule: "record-noncanonical",
            });
        }
        let record_id = hash_object(&record_bytes);
        let supplied: BTreeSet<Hash> = objects.iter().map(StoredObject::hash).collect();
        let declared: BTreeSet<Hash> = record.content_refs.iter().copied().collect();
        if supplied != declared {
            return Err(MediumError::Chain {
                rule: "content-set-mismatch",
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

        // The inserts: every object (insert-once, verify-on-retry),
        // then the record row. An in-window failure takes the rollback
        // path: a clean rollback is the typed error (nothing
        // installed); a failed rollback is Unknown.
        let transactional = append_inserts(
            &tx,
            &self.cursor,
            record,
            objects,
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
        // O(1) work.
        advance_cursor(&mut self.cursor, record, &record_id);
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
}

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
                // never assumed reclamation).
                if record_exists(conn, &parent)? {
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
/// already-closed handle).
fn walk_suffix(
    conn: &Connection,
    target: Hash,
    until: Hash,
    trusted_epoch: u64,
    trusted_keys: BTreeSet<Vec<u8>>,
    trusted_record_ids: BTreeSet<Hash>,
    reclaimed: BTreeMap<Hash, u64>,
) -> Result<Vec<(Hash, RevisionRecord)>, MediumError> {
    let mut suffix: Vec<(Hash, RevisionRecord)> = Vec::new();
    let mut current = target;
    let mut newer_epoch: Option<u64> = None;
    let mut visited = BTreeSet::new();
    let mut suffix_keys = BTreeSet::new();
    loop {
        if current == until {
            if let Some((_, boundary)) = suffix.last()
                && trusted_epoch.checked_add(1) != Some(boundary.epoch)
            {
                return Err(MediumError::Chain {
                    rule: "epoch-regression",
                });
            }
            let mut positions: BTreeMap<Hash, usize> = BTreeMap::new();
            for (index, (id, _)) in suffix.iter().enumerate() {
                positions.insert(*id, index);
            }
            for (index, (_, record)) in suffix.iter().enumerate() {
                for dependency in &record.depends_on {
                    let resolves = trusted_record_ids.contains(dependency)
                        || positions
                            .get(dependency)
                            .is_some_and(|position| *position > index)
                        || reclaimed
                            .get(dependency)
                            .is_some_and(|&dep_epoch| dep_epoch < record.epoch);
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
        for reference in &record.content_refs {
            load_referenced_object(conn, reference)?;
        }
        newer_epoch = Some(record.epoch);
        match record.parent {
            Some(parent) => {
                suffix.push((current, record));
                current = parent;
            }
            None => return Err(MediumError::Chain { rule: "divergence" }),
        }
    }
}

/// Merges a verified suffix (newest-first) into the cursor and points
/// it at the new head — oldest-first absorption so the final epoch is
/// the newest verified one.
fn absorb_suffix(
    cursor: &mut Option<SqliteCursor>,
    suffix: Vec<(Hash, RevisionRecord)>,
    new_head: Hash,
) {
    let Some(cursor) = cursor.as_mut() else {
        return;
    };
    for (id, record) in suffix.into_iter().rev() {
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
        cursor.epoch = record.epoch;
    }
    cursor.head = new_head;
}

/// Advances the cursor over an accepted append — O(1) work.
fn advance_cursor(cursor: &mut Option<SqliteCursor>, record: &RevisionRecord, id: &Hash) {
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
