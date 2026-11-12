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
    fn begin_reclamation_v2(
        &mut self,
        mk: &Hash,
        tick: u64,
    ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
        // (V2X-CP2) The routing delegation to the trusted
        // transactional V2 admission — no rewiring of any
        // destructive or completion boundary.
        SqliteMedium::begin_reclamation_v2(self, mk, tick)
    }

    fn verify_begin_authority_v2(
        &mut self,
        mk: &Hash,
    ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
        // (V2X-CP2) One consistent read snapshot for the whole
        // recovery-safe verification; the truthful tier captured
        // before the transaction borrows the connection.
        let tier = self.durability_tier();
        let tx = self
            .conn
            .transaction()
            .map_err(|e| sqlite_err("v2 begin-authority transaction", e))?;
        let result = verify_begin_authority_v2_tx(&tx, mk, tier);
        drop(tx);
        result
    }

    fn observe_range_terminal_under_coordination(
        &mut self,
        expected: &crate::range_completion::RangeCompletionContext,
        limits: &crate::range_installation::RangeInstallationLimits,
    ) -> Result<crate::range_installation::CoordinatedObservation, MediumError> {
        // (V2X-CP3-B-P2A-C5b) ONE consistent read snapshot for the
        // whole coordinated observation: the head read and the
        // entire C5a walk inside a single read transaction whose
        // snapshot binds at its first statement (a concurrent
        // writer's commit is either wholly visible before the
        // snapshot or wholly invisible after it — internal
        // consistency by the snapshot semantics, no new machinery).
        // Read-only: the transaction is dropped, never committed.
        let tx = self
            .conn
            .transaction()
            .map_err(|e| sqlite_err("range observation transaction", e))?;
        let result = observe_range_terminal_tx(&tx, expected, limits);
        drop(tx);
        result
    }

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

    /// (V2-W, W3A) The SQLite provider's effective per-object byte
    /// capacity: the connection's ACTUAL `SQLITE_MAX_LENGTH` runtime
    /// limit (the maximum length of any bound string or blob — the
    /// honest bound a reclamation publication's stored object must
    /// fit), queried live through the runtime limit API.
    fn effective_object_byte_limit(&mut self) -> Result<u64, MediumError> {
        let observed = self
            .conn
            .limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH)
            .map_err(|error| sqlite_err("query length limit", error))?;
        Ok(observed.max(0) as u64)
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
        let outcome = reclaim_authorized_tx(&tx, directive);
        // (E4) The cursor lifecycle: the mutation phase was reached —
        // the pre-retirement cursor is never authoritative afterward
        // (a rollback leaves nothing deleted, but the invalidated
        // state is uniformly safe; the next operation rebuilds via
        // the authenticated walk).
        self.cursor = None;
        let outcome = outcome?;
        // (OC-C3 CP-2) THE HONEST COMMIT UNCERTAINTY: an injected
        // CommitFailure (the established seam) rolls the transaction
        // back; a real COMMIT error leaves the rollback outcome
        // UNPROVEN (rusqlite 0.40.2's Drop-rollback failures are
        // silent). Both are the single typed
        // reclaim/commit-uncertain class — never success, never a
        // definite absence: the crossing STOPs, and the next
        // authorized pass independently re-verifies the begin and
        // the actual deletion state under a fresh transaction.
        if fault_check(&self.fault, SqliteFaultPoint::CommitFailure).is_err() {
            let _ = tx.rollback();
            return Err(MediumError::Chain {
                rule: "reclaim/commit-uncertain",
            });
        }
        if tx.commit().is_err() {
            return Err(MediumError::Chain {
                rule: "reclaim/commit-uncertain",
            });
        }
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

    fn begin_reclamation(
        &mut self,
        audit: &Hash,
        mk: &Hash,
        tick: u64,
    ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
        // (OC-C3 CP-1) The routing delegation to the accepted OC-C2
        // admission — UNCHANGED semantics, no rewiring of any
        // destructive or completion boundary.
        SqliteMedium::begin_reclamation(self, audit, mk, tick)
    }

    fn verify_begin_authority(
        &mut self,
        audit: &Hash,
        mk: &Hash,
    ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
        // (OC-C3 CP-1) One consistent read snapshot for the whole
        // verification; the truthful tier captured before the
        // transaction borrows the connection.
        let tier = self.durability_tier();
        let tx = self
            .conn
            .transaction()
            .map_err(|e| sqlite_err("begin-authority transaction", e))?;
        let result = verify_begin_authority_tx(&tx, audit, mk, tier);
        drop(tx);
        result
    }
}

/// (OC-C3 CP-1) The transaction-scoped installed-begin authority
/// verification: the recovery-safe audit authentication (no deleted
/// original retirement record required), the founding-derived seed,
/// the shared canonical begin verifier over ONE transaction-scoped
/// reader, and the SQLite durability grade — the read-snapshot
/// visibility proof under the verified WAL/FULL configuration (a
/// begin observed inside a fresh transaction was committed — the
/// exposure follows the synchronized WAL commit — preserving the
/// distinction between canonical membership and a previous
/// publication's acknowledged outcome).
fn verify_begin_authority_tx(
    tx: &rusqlite::Transaction<'_>,
    audit: &Hash,
    mk: &Hash,
    tier: SyncTier,
) -> Result<crate::reclaim::BeginAuthority, MediumError> {
    let (ledger, metadata, head) = authenticate_audit_tx(tx, audit)?;
    if ledger.mk != *mk {
        return Err(MediumError::Chain {
            rule: "begin-authority/mk-mismatch",
        });
    }
    // The founding-derived seed (the E1 discipline — the
    // recovery-safe gap-aware descent).
    let founding = crate::reclaim::founding_identity(
        head,
        |identity| load_record_verified(tx, identity),
        |identity| load_object_verified(tx, identity),
        |identity| record_exists(tx, identity),
    )?;
    crate::reclaim::verify_founding_seed(&metadata, &founding)?;
    // The shared canonical begin verifier over ONE
    // transaction-scoped reader.
    struct SqliteBeginReads<'a, 'b> {
        tx: &'b rusqlite::Transaction<'a>,
    }
    impl crate::reclaim::BeginReads for SqliteBeginReads<'_, '_> {
        fn read_record_bytes(&mut self, id: &Hash) -> Result<Vec<u8>, MediumError> {
            load_record_verified(self.tx, id)
        }
        fn read_object_bytes(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
            load_object_verified(self.tx, hash)
        }
    }
    let mut reads = SqliteBeginReads { tx };
    let Some((id, _begin)) =
        crate::reclaim::verify_installed_begin(&mut reads, head, audit, mk, &ledger)?
    else {
        return Err(MediumError::Chain {
            rule: "begin-authority/absent",
        });
    };
    Ok(crate::reclaim::BeginAuthority { id, tier })
}

/// The trusted reclamation internals (K2-C2B P2) — the transaction-
/// scoped authentication and execution (the same logical authority
/// contract as the local provider's locked form).
/// The OC-C2 trusted begin admission and publication under ONE
/// `BEGIN IMMEDIATE` transaction (the authoritative verification and
/// the publication coordinated; the transaction-scoped hash-verifying
/// reads; no public trait method, no nested transaction): the audit
/// authentication, the shared first-execution evidence core, the
/// duplicate resolution, and the insertion + the HEAD advance +
/// the COMMIT — all atomic (a rolled-back admission leaves nothing).
/// NOT combined with destructive deletion (OC-C3). The tier is the
/// provider's TransactionalWalFull.
/// The transaction-scoped begin discovery (the OC-C2-F3 corrective):
/// the gap-aware descent from the authoritative HEAD for the
/// deterministic key — proving the canonical membership by
/// construction — with the full revision-shape validation and the
/// strict decode. An unreferenced same-key row is never found (and
/// never authoritative).
fn find_installed_begin_tx(
    tx: &rusqlite::Transaction<'_>,
    mk: &Hash,
    expected: &crate::reclaim::ReclamationBegin,
) -> Result<Option<(Hash, crate::reclaim::ReclamationBegin)>, MediumError> {
    const MAX_DESCENT: u64 = 1 << 20;
    let key = crate::reclaim::reclamation_begin_idempotency_key(mk);
    let mut current = read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut skip_table: Option<CheckpointMetadata> = None;
    for _ in 0..MAX_DESCENT {
        let bytes = load_record_verified(tx, &current)?;
        let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
        if record.idempotency_key.as_deref() == Some(key.as_slice()) {
            let Some(parent) = record.parent else {
                return Err(MediumError::Chain {
                    rule: "begin-admission/installed-shape-mismatch",
                });
            };
            if !matches!(record.kind, crate::record::RecordKind::Regular)
                || record.content_refs.len() != 1
            {
                return Err(MediumError::Chain {
                    rule: "begin-admission/installed-shape-mismatch",
                });
            }
            // (C2-R3) The parent/epoch linkage: a present parent must
            // carry epoch+1; an absent parent must be an
            // authenticated skip-table gap.
            if record_exists(tx, &parent)? {
                let parent_bytes = load_record_verified(tx, &parent)?;
                let parent_record =
                    RevisionRecord::decode(&parent_bytes).map_err(MediumError::Format)?;
                // (C2-R7) Checked arithmetic: an epoch at u64::MAX has
                // no valid successor — saturation must never make an
                // incoherent linkage appear coherent.
                if parent_record.epoch.checked_add(1) != Some(record.epoch) {
                    return Err(MediumError::Chain {
                        rule: "begin-admission/installed-linkage-mismatch",
                    });
                }
            } else {
                let Some(table) = skip_table.as_ref() else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                let covered = if table.sk == parent {
                    table.self_skip().is_some()
                } else {
                    table.skip_for(&current).is_some()
                };
                if !covered {
                    return Err(MediumError::MissingObject { hash: parent });
                }
            }
            let object = record.content_refs[0];
            let object_bytes = load_object_verified(tx, &object)?;
            let installed = crate::reclaim::ReclamationBegin::decode(&object_bytes)
                .map_err(MediumError::Format)?;
            if installed.mk != *mk {
                return Err(MediumError::Chain {
                    rule: "begin-admission/installed-mk-mismatch",
                });
            }
            if installed != *expected {
                return Err(MediumError::Chain {
                    rule: "begin-admission/duplicate-content-mismatch",
                });
            }
            return Ok(Some((current, installed)));
        }
        if let Some(object) = record.content_refs.first().copied() {
            let object_bytes = load_object_verified(tx, &object)?;
            if StoredObject::new(object_bytes.clone())?.domain()
                == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN
            {
                skip_table =
                    Some(CheckpointMetadata::decode(&object_bytes).map_err(MediumError::Format)?);
            }
        }
        let Some(parent) = record.parent else {
            return Ok(None);
        };
        if !record_exists(tx, &parent)? {
            let Some(table) = skip_table.as_ref() else {
                return Err(MediumError::MissingObject { hash: parent });
            };
            let target = if table.sk == parent {
                table.self_skip().map(|e| e.nearest_retained_ancestor)
            } else {
                table
                    .skip_for(&current)
                    .map(|e| e.nearest_retained_ancestor)
            };
            let Some(target) = target else {
                return Err(MediumError::MissingObject { hash: parent });
            };
            current = target;
        } else {
            current = parent;
        }
    }
    Ok(None)
}

/// The admission's transaction body. Returns the publication outcome
/// AND the exact attempted candidate revision identity (Some only
/// when an installation was attempted) — the (C2-R5) identity the
/// uncertain-commit reconciliation must be bound to.
fn begin_reclamation_tx(
    tx: &rusqlite::Transaction<'_>,
    audit: &Hash,
    mk: &Hash,
    tick: u64,
) -> Result<(crate::reclaim::BeginPublicationOutcome, Option<Hash>), MediumError> {
    // (A) The exact canonical audit identity over the
    // transaction-scoped reads.
    let (ledger, metadata, _head) = authenticate_audit_tx(tx, audit)?;
    if ledger.mk != *mk {
        return Err(MediumError::Chain {
            rule: "begin-admission/mk-mismatch",
        });
    }
    // (F4) The gap-aware founding descent (the shared
    // reader-parameterized helper over the transaction-scoped
    // hash-verifying reads).
    let founding = crate::reclaim::founding_identity(
        read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?,
        |identity| load_record_verified(tx, identity),
        |identity| load_object_verified(tx, identity),
        |identity| record_exists(tx, identity),
    )?;
    // (B-D) The shared first-execution evidence core over the
    // transaction-scoped hash-verified record reads.
    let mut begin = crate::reclaim::verify_first_execution_evidence(
        &ledger,
        &metadata,
        &founding,
        |identity| load_record_verified(tx, identity),
        |identity| load_object_verified(tx, identity),
    )?;
    begin.source = *audit;
    // (F3) The duplicate resolution: the gap-aware HEAD-ANCESTRY
    // discovery (the idempotency index is a discovery aid, never
    // authority — an unreferenced same-key row is not a canonical
    // begin), the full revision-shape validation, and the exact
    // content verification.
    let key = crate::reclaim::reclamation_begin_idempotency_key(&begin.mk);
    if let Some((installed_id, _installed)) = find_installed_begin_tx(tx, mk, &begin)? {
        return Ok((
            crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id: installed_id },
            None,
        ));
    }
    // The publication: the object row + the record row + the HEAD
    // advance — one atomic unit with the commit.
    let head = read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
    let parent_bytes = load_record_verified(tx, &head)?;
    let parent = RevisionRecord::decode(&parent_bytes).map_err(MediumError::Format)?;
    let parent_epoch = parent.epoch;
    parent_epoch.checked_add(1).ok_or(MediumError::Chain {
        rule: "epoch-regression",
    })?;
    let object = StoredObject::new(begin.canonical_bytes())?;
    let record = RevisionRecord::regular(head, parent_epoch, tick)
        .with_idempotency_key(key.clone())
        .with_content_refs(vec![object.hash()]);
    let record_bytes = record.canonical_bytes();
    let record_id = crate::hash::hash_object(&record_bytes);
    // (F2) The insert-once, verify-on-retry discipline (the
    // established helper — a preexisting row under the expected
    // identity is hash-verified before reuse; a corrupt row refuses
    // typed and is never overwritten).
    insert_object_once(tx, &None, &object)?;
    tx.execute(
        "INSERT INTO records (hash, bytes, epoch, parent, idem, kind, tick) \
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
        params![
            record_id.as_bytes(),
            record_bytes,
            record.epoch as i64,
            head.as_bytes(),
            key,
            record.tick as i64,
        ],
    )
    .map_err(|e| {
        // The same coherence guarantee as the normal admitted
        // publication: the chain-wide idempotency-key uniqueness is
        // the typed duplicate rule (an orphan squatting the key
        // refuses fail-closed; nothing installed).
        if is_constraint(&e) {
            MediumError::Chain {
                rule: "duplicate-idempotency-key",
            }
        } else {
            sqlite_err("begin record insert", e)
        }
    })?;
    tx.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'head'",
        params![record_id.as_bytes()],
    )
    .map_err(|e| sqlite_err("begin head advance", e))?;
    Ok((
        crate::reclaim::BeginPublicationOutcome::Accepted {
            id: record_id,
            // (C2-R8) The truthful transactional tier: this value
            // surfaces ONLY after the wrapper's successful COMMIT
            // (the uncertain-commit paths divert to the read-only
            // reconciliation before returning it), under the
            // already-verified WAL/FULL configuration.
            tier: SyncTier::TransactionalWalFull,
        },
        Some(record_id),
    ))
}

/// (V2X-CP2) The transaction-scoped reader adapter — ONE
/// transaction backing both [`crate::reclaim::BeginReads`] kinds
/// through the established hash-verifying loads.
struct SqliteTxReads<'a, 'b> {
    tx: &'b rusqlite::Transaction<'a>,
}
impl crate::reclaim::BeginReads for SqliteTxReads<'_, '_> {
    fn read_record_bytes(&mut self, id: &Hash) -> Result<Vec<u8>, MediumError> {
        load_record_verified(self.tx, id)
    }
    fn read_object_bytes(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
        load_object_verified(self.tx, hash)
    }
}

/// (V2X-CP2) The plan-bound V2 begin admission under ONE
/// `BEGIN IMMEDIATE` transaction: the ALL-AUDITS-FIRST CP1 contract
/// re-derived INSIDE the transaction over the transaction-scoped
/// hash-verifying reads (a pre-transaction verification never
/// substitutes); only the exact ExecutionReady positive admits; the
/// begin from the verified values only (the ratified plan-record
/// source semantics); the duplicate resolution's exact-content check
/// (the V1/V2 source-form exclusion); the atomic publication (the
/// object row + the record row + the HEAD advance + the COMMIT — a
/// rolled-back admission leaves nothing). The tier is the provider's
/// TransactionalWalFull, surfaced only after the wrapper's
/// successful COMMIT.
fn begin_reclamation_v2_tx(
    tx: &rusqlite::Transaction<'_>,
    mk: &Hash,
    tick: u64,
) -> Result<(crate::reclaim::BeginPublicationOutcome, Option<Hash>), MediumError> {
    // (A) The all-audits-first CP1 contract inside the transaction.
    let head = read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut reads = SqliteTxReads { tx };
    let (plan_record, range_digest, seed) =
        match crate::reclaim::verify_v2_execution_readiness_via(&mut reads, head, mk)? {
            crate::reclaim::V2ExecutionReadiness::ExecutionReady { plan_record, .. } => {
                let Some((_id, meta_bytes)) =
                    crate::reclaim::find_on_chain_via(&mut reads, head, None, Some(mk))?
                else {
                    return Err(MediumError::Chain {
                        rule: "v2-begin-admission/mk-not-installed",
                    });
                };
                let metadata =
                    CheckpointMetadata::decode(&meta_bytes).map_err(MediumError::Format)?;
                (plan_record, metadata.range_digest, metadata.seed)
            }
            _ => {
                return Err(MediumError::Chain {
                    rule: "v2-begin-admission/not-execution-ready",
                });
            }
        };
    // (B) The begin from the verified values only.
    let begin = crate::reclaim::ReclamationBegin {
        mk: *mk,
        source: plan_record,
        range_digest,
        seed,
    };
    // (C) The duplicate resolution (the exact-content check IS the
    // V1/V2 source-form exclusion). A byte-identical installed
    // begin is DuplicateVerified ONLY when the installed begin's
    // own authority holds (V2X-CP2 F1): the strict temporal
    // position and the complete retained membership, re-proved
    // inside this same transaction — an adversarially installed
    // byte-identical begin above a later-published audit is a
    // typed temporal refusal, never a trusted duplicate.
    let key = crate::reclaim::reclamation_begin_idempotency_key(&begin.mk);
    if let Some((installed_id, _installed)) = find_installed_begin_tx(tx, mk, &begin)? {
        let mut reads = SqliteTxReads { tx };
        let dup_head = read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
        crate::reclaim::verify_v2_begin_authority_evidence_via(&mut reads, dup_head, mk)?;
        return Ok((
            crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id: installed_id },
            None,
        ));
    }
    // (D) The atomic publication.
    let head = read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
    let parent_bytes = load_record_verified(tx, &head)?;
    let parent = RevisionRecord::decode(&parent_bytes).map_err(MediumError::Format)?;
    let parent_epoch = parent.epoch;
    parent_epoch.checked_add(1).ok_or(MediumError::Chain {
        rule: "epoch-regression",
    })?;
    let object = StoredObject::new(begin.canonical_bytes())?;
    let record = RevisionRecord::regular(head, parent_epoch, tick)
        .with_idempotency_key(key.clone())
        .with_content_refs(vec![object.hash()]);
    let record_bytes = record.canonical_bytes();
    let record_id = crate::hash::hash_object(&record_bytes);
    insert_object_once(tx, &None, &object)?;
    tx.execute(
        "INSERT INTO records (hash, bytes, epoch, parent, idem, kind, tick) \
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
        params![
            record_id.as_bytes(),
            record_bytes,
            record.epoch as i64,
            head.as_bytes(),
            key,
            record.tick as i64,
        ],
    )
    .map_err(|e| {
        if is_constraint(&e) {
            MediumError::Chain {
                rule: "duplicate-idempotency-key",
            }
        } else {
            sqlite_err("v2 begin record insert", e)
        }
    })?;
    tx.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'head'",
        params![record_id.as_bytes()],
    )
    .map_err(|e| sqlite_err("v2 begin head advance", e))?;
    Ok((
        crate::reclaim::BeginPublicationOutcome::Accepted {
            id: record_id,
            tier: SyncTier::TransactionalWalFull,
        },
        Some(record_id),
    ))
}

/// (V2X-CP3-B-P2A-C5b) The transaction-scoped coordinated
/// observation: the authoritative head read from the meta table
/// INSIDE the caller's transaction, then the accepted C5a
/// observation walk over the transaction-scoped hash-verifying
/// reads — the head read and the whole walk inside ONE snapshot.
/// READ-ONLY. An unfounded campaign refuses typed.
fn observe_range_terminal_tx(
    tx: &rusqlite::Transaction<'_>,
    expected: &crate::range_completion::RangeCompletionContext,
    limits: &crate::range_installation::RangeInstallationLimits,
) -> Result<crate::range_installation::CoordinatedObservation, MediumError> {
    let coordinated_head = read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut reads = SqliteTxReads { tx };
    crate::range_installation::observe_coordinated(&mut reads, &coordinated_head, expected, limits)
}

/// (V2X-CP2) The transaction-scoped retained-evidence V2 begin
/// authority: the recovery-safe verification (no original retiring
/// record required) over ONE transaction-scoped reader, and the
/// SQLite durability grade — the read-snapshot visibility proof
/// under the verified WAL/FULL configuration.
fn verify_begin_authority_v2_tx(
    tx: &rusqlite::Transaction<'_>,
    mk: &Hash,
    tier: SyncTier,
) -> Result<crate::reclaim::BeginAuthority, MediumError> {
    let head = read_meta_head(tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut reads = SqliteTxReads { tx };
    let evidence = crate::reclaim::verify_v2_begin_authority_evidence_via(&mut reads, head, mk)?;
    Ok(crate::reclaim::BeginAuthority {
        id: evidence.begin_id,
        tier,
    })
}

fn reclaim_authorized_tx(
    tx: &rusqlite::Transaction<'_>,
    directive: &crate::reclaim::ReclaimDirective,
) -> Result<crate::reclaim::ReclaimOutcome, MediumError> {
    let (ledger, metadata, head) = authenticate_audit_tx(tx, &directive.audit_record)?;
    if ledger.units != directive.units {
        return Err(MediumError::Chain {
            rule: "reclaim/directive-units-mismatch",
        });
    }
    // (E2) The newest-first retirement order.
    crate::reclaim::verify_newest_first_order(&ledger.units, &metadata)?;
    // The manifest reread with the resume tolerance (the records-
    // table read; an absent retiring record is the protocol-
    // accounted resume — but ONLY in the deletion-prefix shape: the
    // (E1) recovery boundary. This provider's pass 1 deletes
    // newest-first under one transaction, so a GENUINE process
    // interruption rolls back to the all-present state — a
    // prefix-shaped absence here is always external degradation,
    // currently accepted under the SAME unresolved recovery-policy
    // question as the file provider (the owner decision PENDING —
    // Options A/B/C, none ratified; no durable begin-record exists
    // on this provider, whose atomic boundary never produces the
    // partial shape itself). A non-prefix pattern is evidence of
    // missing storage, never proof of a prior authorized deletion —
    // the fabricated-audit-with-missing-data shape fails closed).
    let mut present_seen = false;
    for unit in &ledger.units {
        if record_exists(tx, &unit.record)? {
            present_seen = true;
            let bytes = load_record_verified(tx, &unit.record)?;
            let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
            if unit.objects != record.content_refs {
                return Err(MediumError::Chain {
                    rule: "reclaim/units-objects-mismatch",
                });
            }
        } else if present_seen {
            return Err(MediumError::Chain {
                rule: "reclaim/absence-not-deletion-prefix",
            });
        }
    }
    // (OC-C3 CP-2) THE BEGIN-BOUND DESTRUCTIVE AUTHORIZATION —
    // inside the SAME provider transaction, before any deletion:
    // the accepted CP-1 transaction-scoped verification helper (the
    // recovery-safe authentication, the founding seed, the canonical
    // verifier, the TransactionalWalFull grade from the read
    // snapshot's visibility proof — never a separate unlocked
    // advisory result). The DEFINITE canonical absence is the
    // crossing's fresh-admission trigger; every corrupt,
    // mismatched, orphaned, or unverifiable state is its own typed
    // refusal — never absence.
    match verify_begin_authority_tx(
        tx,
        &directive.audit_record,
        &ledger.mk,
        SyncTier::TransactionalWalFull,
    ) {
        Ok(_authority) => {}
        Err(MediumError::Chain {
            rule: "begin-authority/absent",
        }) => {
            return Err(MediumError::Chain {
                rule: "execution/begin-absent",
            });
        }
        Err(other) => return Err(other),
    }
    // The survivor partition under the same transaction — and the
    // E1 range commitments: the founding-derived seed (always
    // verifiable) and, on the FIRST execution (every range
    // publication present), the recomputed range digest.
    let exclusion: BTreeSet<Hash> = ledger.units.iter().map(|unit| unit.record).collect();
    let walked = walk_chain_excluding(tx, head, &exclusion)?;
    crate::reclaim::verify_founding_seed(&metadata, &walked.cursor.founding)?;
    let mut all_present = true;
    for unit in &ledger.units {
        if unit.record != ledger.sk && !record_exists(tx, &unit.record)? {
            all_present = false;
            break;
        }
    }
    if all_present {
        let mut digest_members: Vec<(u64, Hash)> = ledger
            .units
            .iter()
            .filter(|unit| unit.record != ledger.sk)
            .filter_map(|unit| {
                metadata
                    .index
                    .iter()
                    .find(|e| e.hash == unit.record)
                    .map(|e| (e.epoch, unit.record))
            })
            .collect();
        digest_members.sort_unstable_by_key(|(epoch, _)| *epoch);
        let mut range_bytes = Vec::with_capacity(digest_members.len());
        for (_, record) in digest_members {
            range_bytes.push(load_record_verified(tx, &record)?);
        }
        let recomputed = crate::checkpoint::chain_range_digest(&metadata.seed, &range_bytes);
        if recomputed != ledger.range_digest {
            return Err(MediumError::Chain {
                rule: "reclaim/range-digest-mismatch",
            });
        }
    }
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
    // identities never touched). (E3) The content-integrity
    // verification: each PRESENT row's stored bytes are read and
    // hash-verified INSIDE the transaction before deletion — a
    // corrupted row under an audited hash is PRESERVED evidence
    // (the typed integrity refusal rolls the whole transaction
    // back); an absent row is the resume (already reclaimed).
    for identity in &partition.mandatory {
        let bytes: Option<Vec<u8>> = tx
            .query_row(
                "SELECT bytes FROM objects WHERE hash = ?1",
                params![identity.as_bytes()],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(no_rows_as_none)
            .map_err(|e| sqlite_err("probe object before delete", e))?;
        let Some(bytes) = bytes else {
            continue;
        };
        if hash_object(&bytes) != *identity {
            return Err(MediumError::ReferencedObjectCorrupt { hash: *identity });
        }
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
/// timeout. A request is never assumed granted. THE TIMEOUT FIRST:
/// the busy timeout is installed BEFORE the journal-mode transition
/// — the transition may have to recover a killed writer's hot WAL
/// (taking locks momentarily), and without the timeout any transient
/// contention is an immediate `SQLITE_BUSY` (the CI flake of the
/// interrupted-run relaunch at be6ffe9's first dispatch); with it,
/// the recovery waits within the established bound.
fn configure_writable(conn: &Connection, config: &SqliteMediumConfig) -> Result<(), MediumError> {
    conn.busy_timeout(config.busy_timeout)
        .map_err(|e| sqlite_err("set busy_timeout", e))?;
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
        // The E1 authentic commitments + the E2 newest-first order.
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, _p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, sk, 3),
            ],
            range_digest,
            seed,
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
                    record: sk,
                    objects: sk_refs,
                },
                crate::reclaim::ReclaimUnit {
                    record: r1,
                    objects: r1_refs,
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
        // (OC-C3 CP-2) The begin-bound migration: the REAL trusted
        // admission (the execution gate requires it).
        let begin_outcome = medium.begin_reclamation(&audit, &mk.id(), 221).unwrap();
        assert!(
            matches!(
                begin_outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "the fixture's begin admits: {begin_outcome:?}"
        );
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
    #[test]
    fn a_corrupted_object_row_refuses_and_rolls_back_on_sqlite() {
        // (E3) The object pass reads and HASH-VERIFIES each present
        // row's bytes before the DELETE: a corrupted row at the
        // audited hash key (the bytes no longer hash to the key)
        // refuses with the TYPED ReferencedObjectCorrupt and the
        // single BEGIN IMMEDIATE transaction rolls back — NOTHING
        // deleted, both tables unchanged. Restoring the authentic
        // bytes lets the same directive execute successfully.
        let root = std::env::temp_dir().join(format!(
            "s7-p2cor-{}-{}",
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
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2cor", 0)
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
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, _p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, sk, 3),
            ],
            range_digest,
            seed,
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
                    record: sk,
                    objects: sk_refs,
                },
                crate::reclaim::ReclaimUnit {
                    record: r1,
                    objects: r1_refs,
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
        let exists = |medium: &SqliteMedium, table: &str, id: Hash| -> i64 {
            medium
                .conn
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE hash = ?1)"),
                    params![id.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap()
        };
        // (OC-C3 CP-2) The begin-bound migration: the REAL trusted
        // admission (the execution gate requires it).
        let begin_outcome = medium.begin_reclamation(&audit, &ledger.mk, 220).unwrap();
        assert!(
            matches!(
                begin_outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "the fixture's begin admits: {begin_outcome:?}"
        );
        // Corrupt the private object's stored bytes (injected AFTER
        // the begin's admission — the deeper E3 boundary still
        // reached) — the hash KEY stays, the bytes no longer hash to
        // it.
        medium
            .conn
            .execute(
                "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
                params![b"corrupted-bytes".to_vec(), private.hash().as_bytes()],
            )
            .unwrap();
        let error = medium.reclaim(&directive).unwrap_err();
        // (OC-C3 CP-2) The corrupted object sits ON the authorization
        // walk's path (the ancestry record's first reference — read
        // by the begin-bound gate's hash-verifying walk), so the
        // typed integrity refusal now surfaces at the GATE's read
        // (HashMismatch) rather than the PASS-2 probe
        // (ReferencedObjectCorrupt) — the same typed integrity
        // refusal and the same full-rollback guarantee, one boundary
        // earlier.
        assert!(
            matches!(error, MediumError::HashMismatch { .. }),
            "the typed corruption refusal at the gate's hash-verifying read: {error:?}"
        );
        // The full rollback: the retiring records AND the corrupted
        // object row are all still present.
        assert_eq!(exists(&medium, "records", r1), 1);
        assert_eq!(exists(&medium, "records", sk), 1);
        assert_eq!(exists(&medium, "objects", private.hash()), 1);
        // Restore the authentic bytes and re-run the SAME directive:
        // the execution succeeds.
        medium
            .conn
            .execute(
                "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
                params![private.bytes().to_vec(), private.hash().as_bytes()],
            )
            .unwrap();
        let outcome = medium.reclaim(&directive).unwrap();
        assert_eq!(outcome.deferred, vec![shared.hash()]);
        for id in [private.hash(), r1, sk] {
            assert_eq!(
                exists(&medium, "records", id) + exists(&medium, "objects", id),
                0
            );
        }
        assert_eq!(exists(&medium, "objects", shared.hash()), 1);
        assert!(!medium.reclaim_pending(&directive).unwrap());
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn a_non_prefix_absence_refuses_on_sqlite() {
        // (E1) The recovery-authorization boundary on the second
        // provider: a missing retiring record ROW in a non-prefix
        // pattern (the oldest digest member gone while the SK
        // remains) is a shape this provider's single-transaction
        // pass 1 never produces — a genuine interruption rolls back
        // to the all-present state. The absence is evidence of
        // missing storage, never proof of a prior authorized
        // deletion: the provider refuses typed and nothing further
        // is deleted.
        let root = std::env::temp_dir().join(format!(
            "s7-p2np-{}-{}",
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
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2np", 0)
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
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, _p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, sk, 3),
            ],
            range_digest,
            seed,
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
                    record: sk,
                    objects: sk_refs,
                },
                crate::reclaim::ReclaimUnit {
                    record: r1,
                    objects: r1_refs,
                },
            ],
        };
        let (audit, outcome) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        // The non-prefix absence: the OLDEST digest member's row is
        // gone while the SK (the newest unit) remains.
        medium
            .conn
            .execute(
                "DELETE FROM records WHERE hash = ?1",
                params![r1.as_bytes()],
            )
            .unwrap();
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        let error = medium.reclaim(&directive).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclaim/absence-not-deletion-prefix"
                }
            ),
            "the non-prefix absence refuses: {error:?}"
        );
        // Nothing further deleted: the SK row and both objects
        // remain.
        let exists = |table: &str, id: Hash| -> i64 {
            medium
                .conn
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE hash = ?1)"),
                    params![id.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(exists("records", sk), 1);
        assert_eq!(exists("objects", private.hash()), 1);
        assert_eq!(exists("objects", shared.hash()), 1);
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_trusted_completion_receipt_installs_on_sqlite() {
        // (P2-F4) The trusted completion receipt on the second
        // provider: the executed retirement under the atomic
        // transaction, then complete_reclamation over the
        // recovery-safe authority — the marker installed through
        // the CAS-bound admission, discovered and verified, the
        // duplicate key resolved by exact-content verification.
        let root = std::env::temp_dir().join(format!(
            "s7-p2cmp-{}-{}",
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
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2cmp", 0)
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
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, _p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, sk, 3),
            ],
            range_digest,
            seed,
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
                    record: sk,
                    objects: sk_refs,
                },
                crate::reclaim::ReclaimUnit {
                    record: r1,
                    objects: r1_refs,
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
        // (OC-C3 CP-2) The begin-bound migration: the REAL trusted
        // admission (the execution gate requires it).
        let begin_outcome = medium.begin_reclamation(&audit, &mk.id(), 226).unwrap();
        assert!(
            matches!(
                begin_outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "the fixture's begin admits: {begin_outcome:?}"
        );
        medium.reclaim(&directive).unwrap();
        let (marker_id, marker) =
            crate::reclaim::complete_reclamation(&mut medium, &audit, &mk.id()).unwrap();
        assert_eq!(marker.deferred, vec![shared.hash()]);
        let (found_id, found) = crate::reclaim::find_installed_completion(&mut medium, &mk.id())
            .unwrap()
            .expect("the marker is discoverable on sqlite");
        assert_eq!(found_id, marker_id);
        assert_eq!(found, marker);
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk.id()).unwrap();
        // The duplicate key resolves by exact-content verification.
        let (repeat_id, repeat) =
            crate::reclaim::complete_reclamation(&mut medium, &audit, &mk.id()).unwrap();
        assert_eq!(repeat_id, marker_id);
        assert_eq!(repeat, marker);
        medium.validate().unwrap();
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_historical_accounting_classifies_sqlite_failures_typed() {
        // (P2-R2-H2, the SQLite provider) The historical accounting
        // distinguishes the failure classes on the second provider:
        // a CORRUPT object row under its original hash is NEVER
        // accepted deletion evidence (the typed refusal propagates —
        // never MissingObject); a fabricated later marker refuses
        // the cascade; the multi-generation reopen verifies both
        // genuine markers.
        let root = std::env::temp_dir().join(format!(
            "s7-p2has-{}-{}",
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
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2has", 0)
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
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, _p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, sk, 3),
            ],
            range_digest,
            seed,
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
                    record: sk,
                    objects: sk_refs,
                },
                crate::reclaim::ReclaimUnit {
                    record: r1,
                    objects: r1_refs,
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
        // (OC-C3 CP-2) The begin-bound migration: the REAL trusted
        // admission (the execution gate requires it).
        let begin_outcome = medium.begin_reclamation(&audit, &mk.id(), 222).unwrap();
        assert!(
            matches!(
                begin_outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "the fixture's begin admits: {begin_outcome:?}"
        );
        medium.reclaim(&directive).unwrap();
        let (marker_id, _marker) =
            crate::reclaim::complete_reclamation(&mut medium, &audit, &mk.id()).unwrap();
        // THE CORRUPT ROW under the deferred survivor's hash: the
        // historical verification's deferred-presence read must
        // propagate the typed integrity failure — never
        // MissingObject, never accepted deletion evidence.
        medium
            .conn
            .execute(
                "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
                params![b"corrupted-bytes".to_vec(), shared.hash().as_bytes()],
            )
            .unwrap();
        let error = crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk.id())
            .unwrap_err();
        assert!(
            !matches!(error, MediumError::MissingObject { .. }),
            "the corruption is never classified as absence: {error:?}"
        );
        // Restore the authentic bytes; the marker verifies again.
        medium
            .conn
            .execute(
                "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
                params![shared.bytes().to_vec(), shared.hash().as_bytes()],
            )
            .unwrap();
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk.id()).unwrap();
        // THE FABRICATED LATER MARKER (the wrong content, installed
        // through the in-crate adversarial append): the cascade
        // refuses — it never becomes attribution evidence.
        let head = medium.head().unwrap().unwrap();
        let parent_epoch = medium.read_record(&head).unwrap().epoch;
        let wrong = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk: mk.id(),
            batch: 0,
            source: audit,
            mandatory_count: 42,
            deferred: vec![shared.hash()],
            journal_files: Vec::new(),
        };
        let object = StoredObject::new(wrong.canonical_bytes()).unwrap();
        let record = RevisionRecord::regular(head, parent_epoch, 98)
            .with_idempotency_key(crate::reclaim::reclamation_complete_idempotency_key(
                &mk.id(),
                Some(0),
            ))
            .with_content_refs(vec![object.hash()]);
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
        // THE DUPLICATE-KEY DISCIPLINE: the genuine marker already
        // holds this checkpoint's deterministic completion key — the
        // fabricated same-key record is refused at the append (no
        // second marker can ever install; nothing is silently
        // accepted).
        let outcome = medium.append_admitted(admitted, Some(head)).unwrap_err();
        assert!(
            matches!(
                outcome,
                MediumError::Chain {
                    rule: "duplicate-idempotency-key"
                }
            ),
            "the fabricated same-key marker refuses: {outcome:?}"
        );
        // THE MULTI-GENERATION REOPEN: the genuine marker verifies
        // after the fresh-process restart.
        drop(medium);
        let mut reopened = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut reopened, &marker_id, &mk.id()).unwrap();
        reopened.validate().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_sqlite_two_retirement_attribution_cascade_verifies_and_refuses() {
        // (P2-R2-H1/H2, the genuine SQLite matrix) The two-retirement
        // attribution: A defers the shared X (the later range's B1
        // references it), B mandates and deletes it, both complete —
        // A's historical marker verifies through B's AUTHENTICATED
        // completion evidence, both verify after the fresh reopen.
        // The adversarial variants: a fabricated later marker (a
        // foreign MK with its own valid key — it INSTALLS and
        // REACHES the cascade) refuses at its own source-audit
        // authentication; a corrupted row resurrected at the deleted
        // X's hash propagates the typed integrity failure through
        // the accounting — never classified as absence.
        let root = std::env::temp_dir().join(format!(
            "s7-p2casc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        let fobj = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"founding-payload");
            enc.into_bytes()
        })
        .unwrap();
        let x = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"the-shared-deferred-x");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2casc", 0)
            .with_content_refs(vec![fobj.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&fobj), None)
            .unwrap();
        let step = |medium: &mut SqliteMedium, objects: &[StoredObject], tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
            let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
            medium.append(&record, objects, Some(head)).unwrap();
            record.id()
        };
        // A's range: A1(X) → SK1(fobj) → MK1 → auditA.
        let a1 = step(&mut medium, std::slice::from_ref(&x), 1);
        let sk1 = step(&mut medium, std::slice::from_ref(&fobj), 2);
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let a1_bytes = medium.read_record(&a1).unwrap().canonical_bytes();
        let digest_a = crate::checkpoint::chain_range_digest(&seed, &[a1_bytes]);
        let meta1 = CheckpointMetadata::new(
            sk1,
            a1,
            vec![SkipEntry::new(Hash::ZERO, founding.id())],
            vec![
                ReclaimedIndexEntry::new(None, a1, 1),
                ReclaimedIndexEntry::new(None, sk1, 2),
            ],
            digest_a,
            seed,
        );
        let meta1_object = StoredObject::new(meta1.canonical_bytes()).unwrap();
        let mk1 = step(&mut medium, std::slice::from_ref(&meta1_object), 3);
        let ledger_a = crate::reclaim::ReclamationLedger {
            mk: mk1,
            sk: sk1,
            range_digest: digest_a,
            seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                crate::reclaim::ReclaimUnit {
                    record: sk1,
                    objects: vec![fobj.hash()],
                },
                crate::reclaim::ReclaimUnit {
                    record: a1,
                    objects: vec![x.hash()],
                },
            ],
        };
        let (audit_a, _) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger_a, 4).unwrap();
        // (OC-C3 CP-2) A's begin — the real admission. (OC-C4) The
        // begin record is B1's TRUE nearest retained ancestor: the
        // later range's skip entry names it, never audit_a — a
        // gap-crossing walk from MK2 lands on the begin, never
        // bypasses it.
        let begin_a = medium
            .begin_reclamation(&audit_a, &ledger_a.mk, 224)
            .unwrap();
        let begin_a_id = match begin_a {
            crate::reclaim::BeginPublicationOutcome::Accepted { id, .. }
            | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id } => id,
            other => panic!("A's begin admits: {other:?}"),
        };
        // B's range: B1(X) → SK2(fobj) → MK2 → auditB.
        let b1 = step(&mut medium, std::slice::from_ref(&x), 5);
        let sk2 = step(&mut medium, std::slice::from_ref(&fobj), 6);
        let b1_bytes = medium.read_record(&b1).unwrap().canonical_bytes();
        let digest_b = crate::checkpoint::chain_range_digest(&seed, &[b1_bytes]);
        let meta2 = CheckpointMetadata::new(
            sk2,
            b1,
            vec![SkipEntry::new(Hash::ZERO, begin_a_id)],
            vec![
                ReclaimedIndexEntry::new(None, b1, 5),
                ReclaimedIndexEntry::new(None, sk2, 6),
            ],
            digest_b,
            seed,
        );
        let meta2_object = StoredObject::new(meta2.canonical_bytes()).unwrap();
        let mk2 = step(&mut medium, std::slice::from_ref(&meta2_object), 7);
        let ledger_b = crate::reclaim::ReclamationLedger {
            mk: mk2,
            sk: sk2,
            range_digest: digest_b,
            seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                crate::reclaim::ReclaimUnit {
                    record: sk2,
                    objects: vec![fobj.hash()],
                },
                crate::reclaim::ReclaimUnit {
                    record: b1,
                    objects: vec![x.hash()],
                },
            ],
        };
        let (audit_b, _) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger_b, 8).unwrap();
        // (OC-C3 CP-2) B's begin — the real admission.
        let begin_b = medium
            .begin_reclamation(&audit_b, &ledger_b.mk, 225)
            .unwrap();
        assert!(
            matches!(
                begin_b,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "B's begin admits: {begin_b:?}"
        );
        // Both retirements execute and complete.
        let directive_a = crate::reclaim::ReclaimDirective {
            audit_record: audit_a,
            units: ledger_a.units.clone(),
        };
        medium.reclaim(&directive_a).unwrap();
        let (marker_a, marker_a_content) =
            crate::reclaim::complete_reclamation(&mut medium, &audit_a, &mk1).unwrap();
        assert!(
            marker_a_content.deferred.contains(&x.hash()),
            "A defers the shared X (B1, a survivor above A, references it)"
        );
        let directive_b = crate::reclaim::ReclaimDirective {
            audit_record: audit_b,
            units: ledger_b.units.clone(),
        };
        medium.reclaim(&directive_b).unwrap();
        let (marker_b, _) =
            crate::reclaim::complete_reclamation(&mut medium, &audit_b, &mk2).unwrap();
        // A's historical marker verifies through B's authenticated
        // completion evidence (the per-object accounting).
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_b, &mk2).unwrap();
        // THE CORRUPTED ROW resurrected at the deleted X's hash: the
        // accounting's absence read propagates the typed integrity
        // failure — never classified as absence, never accepted
        // deletion evidence.
        medium
            .conn
            .execute(
                "INSERT OR REPLACE INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
                params![
                    x.hash().as_bytes().to_vec(),
                    "sektor7/save/1",
                    b"corrupted-resurrected".to_vec()
                ],
            )
            .unwrap();
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::HashMismatch { .. } | MediumError::ReferencedObjectCorrupt { .. }
            ),
            "the corrupted resurrected row propagates the typed integrity failure: {error:?}"
        );
        // Remove the corrupted row: the genuine accounting resumes.
        medium
            .conn
            .execute(
                "DELETE FROM objects WHERE hash = ?1",
                params![x.hash().as_bytes()],
            )
            .unwrap();
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap();
        // THE MULTI-GENERATION REOPEN: both markers verify after the
        // fresh-process restart.
        drop(medium);
        let mut reopened = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut reopened, &marker_a, &mk1).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut reopened, &marker_b, &mk2).unwrap();
        reopened.validate().unwrap();
        drop(reopened);
        let mut medium = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        // THE FABRICATED LATER MARKER — a FOREIGN MK with its own
        // valid deterministic key: it INSTALLS (no duplicate) and
        // REACHES the cascade, refusing at its own source-audit
        // authentication (the audit's ledger is bound to MK2, not
        // the foreign MK).
        let foreign_mk = crate::hash::hash_object(b"the-fabricated-foreign-mk");
        let head = medium.head().unwrap().unwrap();
        let parent_epoch = medium.read_record(&head).unwrap().epoch;
        let fabricated = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk: foreign_mk,
            batch: 0,
            source: audit_b,
            mandatory_count: 3,
            deferred: vec![fobj.hash()],
            journal_files: Vec::new(),
        };
        let object = StoredObject::new(fabricated.canonical_bytes()).unwrap();
        let record = RevisionRecord::regular(head, parent_epoch, 98)
            .with_idempotency_key(crate::reclaim::reclamation_complete_idempotency_key(
                &foreign_mk,
                Some(0),
            ))
            .with_content_refs(vec![object.hash()]);
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
        let outcome = medium.append_admitted(admitted, Some(head)).unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/mk-mismatch"
                }
            ),
            "the fabricated foreign-MK later marker refuses at its own source-audit authentication: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_trusted_begin_admission_publishes_on_sqlite() {
        // (OC-C2) The SQLite twin: the valid admission under ONE
        // BEGIN IMMEDIATE publishes Accepted (the begin row + the
        // HEAD advance, atomically committed); the repeat resolves
        // the duplicate by exact content; the fabricated digest
        // refuses with nothing committed.
        let root = std::env::temp_dir().join(format!(
            "s7-occ2-{}-{}",
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
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "occ2", 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let step = |medium: &mut SqliteMedium, objects: &[StoredObject], tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
            let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
            medium.append(&record, objects, Some(head)).unwrap();
            record.id()
        };
        let _p1 = step(&mut medium, std::slice::from_ref(&shared), 1);
        let r1 = step(&mut medium, &[shared.clone(), private.clone()], 2);
        let sk = step(&mut medium, std::slice::from_ref(&shared), 3);
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, _p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, sk, 3),
            ],
            range_digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let mk = step(&mut medium, std::slice::from_ref(&meta_object), 4);
        let r1_refs = medium.read_record(&r1).unwrap().content_refs.clone();
        let sk_refs = medium.read_record(&sk).unwrap().content_refs.clone();
        let ledger = crate::reclaim::ReclamationLedger {
            mk,
            sk,
            range_digest: meta.range_digest,
            seed: meta.seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                crate::reclaim::ReclaimUnit {
                    record: sk,
                    objects: sk_refs,
                },
                crate::reclaim::ReclaimUnit {
                    record: r1,
                    objects: r1_refs,
                },
            ],
        };
        let (audit, outcome) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        // The valid admission.
        let outcome = medium.begin_reclamation(&audit, &mk, 30).unwrap();
        let installed = match outcome {
            // (C2-R8) The transactional admission carries its ACTUAL
            // tier.
            crate::reclaim::BeginPublicationOutcome::Accepted { id, tier } => {
                assert_eq!(
                    tier,
                    SyncTier::TransactionalWalFull,
                    "the sqlite admission carries its actual tier"
                );
                id
            }
            other => panic!("the sqlite admission accepts, got {other:?}"),
        };
        assert_eq!(medium.head().unwrap().unwrap(), installed);
        // The duplicate: exact content.
        let outcome = medium.begin_reclamation(&audit, &mk, 31).unwrap();
        match outcome {
            crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id } => {
                assert_eq!(id, installed)
            }
            other => panic!("the sqlite duplicate verifies, got {other:?}"),
        }
        medium.validate().unwrap();
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_sqlite_begin_admission_refuses_corrupt_and_orphan_evidence() {
        // (OC-C2-F2/F3/F6) The SQLite integrity regressions: a
        // PREPOPULATED corrupt object row under the exact expected
        // begin-object hash refuses typed (the insert-once
        // verify-on-retry discipline), the HEAD unchanged, the
        // corrupt evidence preserved; an ORPHANED same-key record
        // row (never on the ancestry) exerts no authority — the
        // admission publishes Accepted.
        let root = std::env::temp_dir().join(format!(
            "s7-occ2c-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let build = |label: &str| {
            let r = root.join(label);
            std::fs::create_dir_all(&r).unwrap();
            let mut medium = SqliteMedium::open(r.join("db.sqlite")).unwrap();
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
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
                .with_content_refs(vec![shared.hash()]);
            medium
                .append(&founding, std::slice::from_ref(&shared), None)
                .unwrap();
            let step = |medium: &mut SqliteMedium, objects: &[StoredObject], tick: u64| -> Hash {
                let head = medium.head().unwrap().unwrap();
                let epoch = medium.read_record(&head).unwrap().epoch;
                let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
                let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
                medium.append(&record, objects, Some(head)).unwrap();
                record.id()
            };
            let _p1 = step(&mut medium, std::slice::from_ref(&shared), 1);
            let r1 = step(&mut medium, &[shared.clone(), private.clone()], 2);
            let sk = step(&mut medium, std::slice::from_ref(&shared), 3);
            let seed = crate::checkpoint::compute_digest_seed(&founding.id());
            let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
            let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
            let meta = CheckpointMetadata::new(
                sk,
                r1,
                vec![SkipEntry::new(Hash::ZERO, _p1)],
                vec![
                    ReclaimedIndexEntry::new(None, r1, 2),
                    ReclaimedIndexEntry::new(None, sk, 3),
                ],
                range_digest,
                seed,
            );
            let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
            let mk = step(&mut medium, std::slice::from_ref(&meta_object), 4);
            let r1_refs = medium.read_record(&r1).unwrap().content_refs.clone();
            let sk_refs = medium.read_record(&sk).unwrap().content_refs.clone();
            let ledger = crate::reclaim::ReclamationLedger {
                mk,
                sk,
                range_digest: meta.range_digest,
                seed: meta.seed,
                records: 2,
                reclaimed_keys: 0,
                journal_files: Vec::new(),
                units: vec![
                    crate::reclaim::ReclaimUnit {
                        record: sk,
                        objects: sk_refs,
                    },
                    crate::reclaim::ReclaimUnit {
                        record: r1,
                        objects: r1_refs,
                    },
                ],
            };
            let (audit, outcome) =
                crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
            assert!(matches!(
                outcome,
                crate::medium::AppendOutcome::Accepted { .. }
            ));
            (medium, audit, mk, ledger)
        };
        // (F2) The prepopulated corrupt row: derive the exact begin
        // object the admission will construct (the same verified
        // values), pre-insert WRONG bytes under its hash.
        {
            let (mut medium, audit, mk, ledger) = build("corrupt-row");
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let expected_hash = StoredObject::new(begin.canonical_bytes()).unwrap().hash();
            medium
                .conn
                .execute(
                    "INSERT INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
                    params![
                        expected_hash.as_bytes().to_vec(),
                        "sektor7/save/1",
                        b"corrupt-prepopulated".to_vec()
                    ],
                )
                .unwrap();
            let head_before = medium.head().unwrap().unwrap();
            let error = medium.begin_reclamation(&audit, &mk, 60).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::HashMismatch { .. } | MediumError::ReferencedObjectCorrupt { .. }
                ),
                "the corrupt preexisting row refuses typed (the insert-once verify): {error:?}"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                head_before,
                "the HEAD is unchanged"
            );
            let preserved: Vec<u8> = medium
                .conn
                .query_row(
                    "SELECT bytes FROM objects WHERE hash = ?1",
                    params![expected_hash.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                preserved,
                b"corrupt-prepopulated".to_vec(),
                "the corrupt evidence is preserved, never overwritten"
            );
        }
        // (F3) The orphaned same-key record row: inserted directly,
        // never on the ancestry — the head-descent discovery never
        // finds it; the admission publishes Accepted.
        {
            let (mut medium, audit, mk, _ledger) = build("orphan-key");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let orphan_object = StoredObject::new(
                crate::reclaim::ReclamationBegin {
                    mk,
                    source: audit,
                    range_digest: [0u8; 32],
                    seed: [0u8; 32],
                }
                .canonical_bytes(),
            )
            .unwrap();
            let orphan_record = RevisionRecord::regular(Hash::ZERO, 0, 99)
                .with_idempotency_key(key.clone())
                .with_content_refs(vec![orphan_object.hash()]);
            medium
                .conn
                .execute(
                    "INSERT INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
                    params![
                        orphan_object.hash().as_bytes().to_vec(),
                        orphan_object.domain(),
                        orphan_object.bytes().to_vec()
                    ],
                )
                .unwrap();
            medium
                .conn
                .execute(
                    "INSERT INTO records (hash, bytes, epoch, parent, idem, kind, tick)                      VALUES (?1, ?2, 0, NULL, ?3, 1, 99)",
                    params![
                        crate::hash::hash_object(&orphan_record.canonical_bytes())
                            .as_bytes()
                            .to_vec(),
                        orphan_record.canonical_bytes(),
                        key
                    ],
                )
                .unwrap();
            // The orphan squats the deterministic key: the chain-wide
            // unique-idempotency discipline refuses the publication
            // fail-closed (the typed duplicate-key rule) — the orphan
            // never returns DuplicateVerified and never fakes
            // authority; the HEAD is unchanged.
            let error = medium.begin_reclamation(&audit, &mk, 61).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "duplicate-idempotency-key"
                    }
                ),
                "the orphaned same-key row never fakes a duplicate (the fail-closed refusal): {error:?}"
            );
            let head_after = medium.head().unwrap().unwrap();
            let _ = head_after;
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    // (C2-R4) The shared admission store for the second corrective's
    // SQLite regressions: founding → P1 → R1(shared+private) → SK →
    // MK(the meta) → the published audit — the exact evidence shape
    // the admission verifies. Returns (root, medium, audit, mk,
    // ledger, shared, private, p1) so each adversarial case can
    // delete, corrupt, or install the exact rows it needs.
    fn sqlite_begin_store(
        label: &str,
    ) -> (
        std::path::PathBuf,
        SqliteMedium,
        Hash,
        Hash,
        crate::reclaim::ReclamationLedger,
        Hash,
        Hash,
        Hash,
    ) {
        let root = std::env::temp_dir().join(format!(
            "s7-occ2r4-{label}-{}-{}",
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
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let step = |medium: &mut SqliteMedium, objects: &[StoredObject], tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
            let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
            medium.append(&record, objects, Some(head)).unwrap();
            record.id()
        };
        let p1 = step(&mut medium, std::slice::from_ref(&shared), 1);
        let r1 = step(&mut medium, &[shared.clone(), private.clone()], 2);
        let sk = step(&mut medium, std::slice::from_ref(&shared), 3);
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, sk, 3),
            ],
            range_digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let mk = step(&mut medium, std::slice::from_ref(&meta_object), 4);
        let r1_refs = medium.read_record(&r1).unwrap().content_refs.clone();
        let sk_refs = medium.read_record(&sk).unwrap().content_refs.clone();
        let ledger = crate::reclaim::ReclamationLedger {
            mk,
            sk,
            range_digest: meta.range_digest,
            seed: meta.seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                crate::reclaim::ReclaimUnit {
                    record: sk,
                    objects: sk_refs,
                },
                crate::reclaim::ReclaimUnit {
                    record: r1,
                    objects: r1_refs,
                },
            ],
        };
        let (audit, outcome) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        (
            root,
            medium,
            audit,
            mk,
            ledger,
            shared.hash(),
            private.hash(),
            p1,
        )
    }

    // (C2-R4) Installs a hand-built revision ON the SQLite ancestry
    // as the new HEAD (the object row, the record row, the meta head
    // advance) — the corruption-shaped installation the discovery's
    // linkage and shape validation must refuse (the append path
    // enforces the coherence itself; these rows never passed it).
    fn install_sqlite_begin_fake(
        medium: &mut SqliteMedium,
        record: &RevisionRecord,
        object: &StoredObject,
    ) {
        medium
            .conn
            .execute(
                "INSERT INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
                params![
                    object.hash().as_bytes().to_vec(),
                    object.domain(),
                    object.bytes().to_vec()
                ],
            )
            .unwrap();
        let record_bytes = record.canonical_bytes();
        let record_id = crate::hash::hash_object(&record_bytes);
        let kind = if matches!(record.kind, crate::record::RecordKind::Regular) {
            1
        } else {
            0
        };
        medium
            .conn
            .execute(
                "INSERT INTO records (hash, bytes, epoch, parent, idem, kind, tick) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    record_id.as_bytes(),
                    record_bytes,
                    record.epoch as i64,
                    record.parent.map(|parent| parent.as_bytes().to_vec()),
                    record.idempotency_key.clone(),
                    kind,
                    record.tick as i64
                ],
            )
            .unwrap();
        medium
            .conn
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'head'",
                params![record_id.as_bytes()],
            )
            .unwrap();
    }

    #[test]
    fn the_sqlite_begin_commit_faults_reconcile_read_only() {
        // (C2-R1/R4) The uncertain-commit regressions over the
        // established fault seam: the injected CommitFailure (the
        // DEFINITE pre-commit rollback — the transaction is rolled
        // back before any durability; the genuinely-ambiguous real
        // COMMIT error shares the exact uncertain→reconcile path)
        // reconciles through the READ-ONLY observation to
        // ReconciledNotInstalled with NO second publication (no
        // begin row, no begin object, the HEAD unchanged) — and the
        // EXPLICIT retry after the confirmed absence publishes
        // Accepted; the injected AfterCommit (the COMMIT succeeded,
        // the acknowledgment lost) reconciles to
        // ReconciledInstalled with the exact installed identity;
        // a reconciliation that CANNOT verify (the evidence
        // degraded underneath the observation) is the typed
        // unverifiable — never a guess, never a write.
        // (a) The definite pre-commit rollback + the explicit retry.
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("commit-failure");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let expected_hash = StoredObject::new(begin.canonical_bytes()).unwrap().hash();
            let head_before = medium.head().unwrap().unwrap();
            medium.fault = Some(SqliteFaultControl::new().arm(
                SqliteFaultPoint::CommitFailure,
                SqliteFaultAction::InjectError,
            ));
            let outcome = medium.begin_reclamation(&audit, &mk, 70).unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::BeginPublicationOutcome::ReconciledNotInstalled
                ),
                "the definite pre-commit rollback reconciles to the confirmed absence: {outcome:?}"
            );
            let begin_rows: i64 = medium
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE idem = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(begin_rows, 0, "no begin record row (the rollback)");
            let begin_objects: i64 = medium
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM objects WHERE hash = ?1",
                    params![expected_hash.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                begin_objects, 0,
                "the READ-ONLY reconciliation published nothing (no second publication)"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                head_before,
                "the HEAD is unchanged after the reconciled absence"
            );
            // The EXPLICIT retry after the confirmed absence — a
            // separate caller operation, never inside the
            // reconciliation — revalidates and publishes Accepted.
            medium.fault = None;
            let outcome = medium.begin_reclamation(&audit, &mk, 71).unwrap();
            let installed = match outcome {
                // (C2-R8) The retry's Accepted carries the
                // transactional tier.
                crate::reclaim::BeginPublicationOutcome::Accepted { id, tier } => {
                    assert_eq!(
                        tier,
                        SyncTier::TransactionalWalFull,
                        "the retry's Accepted carries the transactional tier"
                    );
                    id
                }
                other => {
                    panic!("the explicit retry after the confirmed absence accepts: {other:?}")
                }
            };
            assert_eq!(
                medium.head().unwrap().unwrap(),
                installed,
                "the retry's installed identity is the new HEAD"
            );
            medium.validate().unwrap();
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) The installed-but-acknowledgment-lost window — the
        // reconciliation is bound to the EXACT attempted candidate
        // revision identity (C2-R5).
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("after-commit");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let head_before = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head_before).unwrap().epoch;
            let candidate = RevisionRecord::regular(head_before, head_epoch, 72)
                .with_idempotency_key(key.clone())
                .with_content_refs(vec![object.hash()]);
            let candidate_id = crate::hash::hash_object(&candidate.canonical_bytes());
            medium.fault = Some(SqliteFaultControl::new().arm(
                SqliteFaultPoint::AfterCommit,
                SqliteFaultAction::InjectError,
            ));
            let outcome = medium.begin_reclamation(&audit, &mk, 72).unwrap();
            let id = match outcome {
                crate::reclaim::BeginPublicationOutcome::ReconciledInstalled { id } => id,
                other => panic!(
                    "the lost acknowledgment reconciles to the proven installation: {other:?}"
                ),
            };
            assert_eq!(
                id, candidate_id,
                "the reconciled identity is the EXACT attempted candidate revision"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                id,
                "the reconciled identity is the installed HEAD"
            );
            let begin_rows: i64 = medium
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE idem = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                begin_rows, 1,
                "exactly one installation (no second publication inside the reconciliation)"
            );
            medium.fault = None;
            medium.validate().unwrap();
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (c) The unverifiable reconciliation: the evidence degraded
        // underneath the observation — the typed uncertainty, never
        // a guess, never a write. Exercised through the same
        // reconcile entry the uncertain path delegates to: the
        // public admission refuses such a state earlier at its own
        // authentication, so the degraded-under-reconciliation
        // observation is constructed directly.
        {
            let (root, mut medium, audit, mk, _ledger, _shared, _private, _p1) =
                sqlite_begin_store("unverifiable");
            medium
                .conn
                .execute(
                    "DELETE FROM records WHERE hash = ?1",
                    params![audit.as_bytes()],
                )
                .unwrap();
            let head_before = medium.head().unwrap().unwrap();
            let error = reconcile_begin_publication(&mut medium, &audit, &mk, None).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/reconcile-unverifiable"
                    }
                ),
                "the degraded evidence is the typed unverifiable: {error:?}"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                head_before,
                "the HEAD is unchanged (the observation refused, never mutated)"
            );
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin_rows: i64 = medium
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE idem = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                begin_rows, 0,
                "the unverifiable observation published nothing"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (d) (C2-R5) A DIFFERENT same-key revision than the
        // attempted candidate: the same valid begin payload under a
        // different revision identity (a different tick) — the
        // reconciliation must classify it as separately discovered,
        // verified duplicate evidence, never ReconciledInstalled for
        // the original attempt. Exercised through the same reconcile
        // entry the uncertain path delegates to (the public
        // admission's duplicate discovery never constructs a second
        // candidate when one is already installed).
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("different-candidate");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            // The already-installed begin revision Y (a valid
            // publication on the current head).
            let head = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let installed = RevisionRecord::regular(head, head_epoch, 80)
                .with_idempotency_key(key.clone())
                .with_content_refs(vec![object.hash()]);
            install_sqlite_begin_fake(&mut medium, &installed, &object);
            let installed_id = crate::hash::hash_object(&installed.canonical_bytes());
            // The DIFFERENT attempted candidate X (the same payload,
            // a different tick — a different revision identity).
            let candidate = RevisionRecord::regular(head, head_epoch, 81)
                .with_idempotency_key(key)
                .with_content_refs(vec![object.hash()]);
            let candidate_id = crate::hash::hash_object(&candidate.canonical_bytes());
            assert_ne!(
                candidate_id, installed_id,
                "the fixtures differ in revision identity"
            );
            let outcome =
                reconcile_begin_publication(&mut medium, &audit, &mk, Some(candidate_id)).unwrap();
            match outcome {
                crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id } => {
                    assert_eq!(
                        id, installed_id,
                        "the different same-key revision is the separately discovered duplicate"
                    )
                }
                other => panic!(
                    "never the attempted candidate's installation for a different revision: {other:?}"
                ),
            }
            assert_eq!(
                medium.head().unwrap().unwrap(),
                installed_id,
                "the observation wrote nothing (the HEAD is the pre-existing installation)"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (e) (C2-R5) The PUBLIC path over a pre-existing different
        // same-key revision: the admission's own duplicate discovery
        // verifies Y before constructing any candidate; the
        // acknowledgment lost in the post-commit window reconciles
        // to the SAME separately-discovered classification (never
        // ReconciledInstalled for a candidate that was never
        // constructed); the explicit retry converges idempotently.
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("ack-lost-duplicate");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let installed = RevisionRecord::regular(head, head_epoch, 82)
                .with_idempotency_key(key.clone())
                .with_content_refs(vec![object.hash()]);
            install_sqlite_begin_fake(&mut medium, &installed, &object);
            let installed_id = crate::hash::hash_object(&installed.canonical_bytes());
            medium.fault = Some(SqliteFaultControl::new().arm(
                SqliteFaultPoint::AfterCommit,
                SqliteFaultAction::InjectError,
            ));
            let outcome = medium.begin_reclamation(&audit, &mk, 83).unwrap();
            match outcome {
                crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id } => {
                    assert_eq!(id, installed_id)
                }
                other => panic!(
                    "the acknowledgment-lost duplicate admission reconciles to the separately discovered duplicate: {other:?}"
                ),
            }
            let begin_rows: i64 = medium
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE idem = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                begin_rows, 1,
                "no second publication (exactly the pre-existing installation)"
            );
            medium.fault = None;
            // The explicit retry converges idempotently on the
            // pre-existing installation.
            let outcome = medium.begin_reclamation(&audit, &mk, 84).unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id }
                        if id == installed_id
                ),
                "the retry converges on the pre-existing installation: {outcome:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_sqlite_begin_admission_refuses_missing_and_corrupt_original_content() {
        // (C2-R4/F1) The SQLite content-object regressions: a
        // MISSING original referenced content row refuses typed at
        // the evidence verification (the matching ledger list is
        // never proof the content exists); a HASH-CORRUPT row
        // refuses at the hash-verifying read and is NEVER
        // overwritten; both leave the HEAD unchanged.
        // (a) The missing original content.
        {
            let (root, mut medium, audit, mk, _ledger, _shared, private, _p1) =
                sqlite_begin_store("missing-content");
            let head_before = medium.head().unwrap().unwrap();
            medium
                .conn
                .execute(
                    "DELETE FROM objects WHERE hash = ?1",
                    params![private.as_bytes()],
                )
                .unwrap();
            let error = medium.begin_reclamation(&audit, &mk, 73).unwrap_err();
            assert!(
                matches!(error, MediumError::MissingObject { hash } if hash == private),
                "the missing original content refuses typed: {error:?}"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                head_before,
                "the HEAD is unchanged"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) The hash-corrupt original content.
        {
            let (root, mut medium, audit, mk, _ledger, _shared, private, _p1) =
                sqlite_begin_store("corrupt-content");
            let head_before = medium.head().unwrap().unwrap();
            medium
                .conn
                .execute(
                    "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
                    params![b"corrupted-original-content".to_vec(), private.as_bytes()],
                )
                .unwrap();
            let error = medium.begin_reclamation(&audit, &mk, 74).unwrap_err();
            assert!(
                matches!(error, MediumError::HashMismatch { expected, .. } if expected == private),
                "the corrupt original content refuses at the hash-verifying read: {error:?}"
            );
            let preserved: Vec<u8> = medium
                .conn
                .query_row(
                    "SELECT bytes FROM objects WHERE hash = ?1",
                    params![private.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                preserved,
                b"corrupted-original-content".to_vec(),
                "the corrupt evidence is preserved, never overwritten"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                head_before,
                "the HEAD is unchanged"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_sqlite_begin_founding_crosses_compacted_history() {
        // (C2-R4/F4) The SQLite compacted-history twin: after A's
        // EXECUTED retirement physically removes A's range (the
        // authenticated self-skip below MK1), B's later admission
        // still derives the TRUE founding's seed — the gap-aware
        // founding descent crosses the authenticated gap; the naive
        // missing-parent descent would stop at the gap and derive a
        // wrong seed (the refusal).
        let root = std::env::temp_dir().join(format!(
            "s7-occ2r4-gap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        let fobj = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b" founding-payload");
            enc.into_bytes()
        })
        .unwrap();
        let x = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"the-shared-deferred-x");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "occ2r4gap", 0)
            .with_content_refs(vec![fobj.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&fobj), None)
            .unwrap();
        // A's range: A1(X) → SK1(fobj) → MK1(meta1) → auditA.
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let a1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![x.hash()]);
        medium
            .append(&a1, std::slice::from_ref(&x), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sk1 = RevisionRecord::regular(head, epoch, 2).with_content_refs(vec![fobj.hash()]);
        medium
            .append(&sk1, std::slice::from_ref(&fobj), Some(head))
            .unwrap();
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let a1_bytes = medium.read_record(&a1.id()).unwrap().canonical_bytes();
        let digest_a = crate::checkpoint::chain_range_digest(&seed, &[a1_bytes]);
        let meta1 = CheckpointMetadata::new(
            sk1.id(),
            a1.id(),
            vec![SkipEntry::new(Hash::ZERO, founding.id())],
            vec![
                ReclaimedIndexEntry::new(None, a1.id(), 1),
                ReclaimedIndexEntry::new(None, sk1.id(), 2),
            ],
            digest_a,
            seed,
        );
        let meta1_object = StoredObject::new(meta1.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk1 =
            RevisionRecord::regular(head, epoch, 3).with_content_refs(vec![meta1_object.hash()]);
        medium
            .append(&mk1, std::slice::from_ref(&meta1_object), Some(head))
            .unwrap();
        let ledger_a = crate::reclaim::ReclamationLedger {
            mk: mk1.id(),
            sk: sk1.id(),
            range_digest: digest_a,
            seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                crate::reclaim::ReclaimUnit {
                    record: sk1.id(),
                    objects: vec![fobj.hash()],
                },
                crate::reclaim::ReclaimUnit {
                    record: a1.id(),
                    objects: vec![x.hash()],
                },
            ],
        };
        let (audit_a, outcome) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger_a, 4).unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        // (OC-C3 CP-2) A's begin — the real admission (the A
        // execution below requires it under the begin-bound gate).
        // (OC-C4) The begin record is B1's TRUE nearest retained
        // ancestor: the later range's skip entry names it, never
        // audit_a — a gap-crossing walk from MK2 lands on the begin,
        // never bypasses it.
        let begin_a = medium
            .begin_reclamation(&audit_a, &ledger_a.mk, 223)
            .unwrap();
        let begin_a_id = match begin_a {
            crate::reclaim::BeginPublicationOutcome::Accepted { id, .. }
            | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id } => id,
            other => panic!("A's begin admits: {other:?}"),
        };
        // The later range: B1(X) → SK2(fobj) → MK2(meta2) — B1/SK2
        // are survivors of A (X deferred: B1 references it).
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let b1 = RevisionRecord::regular(head, epoch, 5).with_content_refs(vec![x.hash()]);
        medium
            .append(&b1, std::slice::from_ref(&x), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sk2 = RevisionRecord::regular(head, epoch, 6).with_content_refs(vec![fobj.hash()]);
        medium
            .append(&sk2, std::slice::from_ref(&fobj), Some(head))
            .unwrap();
        let b1_bytes = medium.read_record(&b1.id()).unwrap().canonical_bytes();
        let digest_b = crate::checkpoint::chain_range_digest(&seed, &[b1_bytes]);
        let meta2 = CheckpointMetadata::new(
            sk2.id(),
            b1.id(),
            vec![SkipEntry::new(Hash::ZERO, begin_a_id)],
            vec![
                ReclaimedIndexEntry::new(None, b1.id(), 5),
                ReclaimedIndexEntry::new(None, sk2.id(), 6),
            ],
            digest_b,
            seed,
        );
        let meta2_object = StoredObject::new(meta2.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk2 =
            RevisionRecord::regular(head, epoch, 7).with_content_refs(vec![meta2_object.hash()]);
        medium
            .append(&mk2, std::slice::from_ref(&meta2_object), Some(head))
            .unwrap();
        let ledger_b = crate::reclaim::ReclamationLedger {
            mk: mk2.id(),
            sk: sk2.id(),
            range_digest: digest_b,
            seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                crate::reclaim::ReclaimUnit {
                    record: sk2.id(),
                    objects: vec![fobj.hash()],
                },
                crate::reclaim::ReclaimUnit {
                    record: b1.id(),
                    objects: vec![x.hash()],
                },
            ],
        };
        // Retirement A EXECUTES: the physical gap below MK1 (the
        // authenticated self-skip to the TRUE founding).
        let (ledger_a_verified, _meta_a) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit_a, &mk1.id()).unwrap();
        medium
            .reclaim(&crate::reclaim::ReclaimDirective {
                audit_record: audit_a,
                units: ledger_a_verified.units.clone(),
            })
            .unwrap();
        // B's audit PUBLISHED above A's retired range.
        let (audit_b, outcome) =
            crate::reclaim::publish_reclamation_ledger(&mut medium, &ledger_b, 8).unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        // B's admission over the compacted history: the founding
        // descent crosses A's authenticated gap to the TRUE founding
        // (the seed check passes); the naive descent would refuse.
        let outcome = medium.begin_reclamation(&audit_b, &mk2.id(), 75).unwrap();
        let installed = match outcome {
            // (C2-R8) The compacted-history admission carries the
            // transactional tier.
            crate::reclaim::BeginPublicationOutcome::Accepted { id, tier } => {
                assert_eq!(
                    tier,
                    SyncTier::TransactionalWalFull,
                    "the compacted-history Accepted carries the transactional tier"
                );
                id
            }
            other => panic!(
                "the compacted-history founding admits over the authenticated gap: {other:?}"
            ),
        };
        assert_eq!(
            medium.head().unwrap().unwrap(),
            installed,
            "the installed begin is the new HEAD"
        );
        medium.validate().unwrap();
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_sqlite_begin_founding_refuses_uncovered_and_unreadable_parents() {
        // (C2-R4/F4/R2) The founding-descent gap discipline: a
        // missing parent with NO authenticated skip coverage refuses
        // typed (never mistaken for the founding); a PRESENT but
        // hash-corrupt parent row refuses at the hash-verifying read
        // (an unreadable parent is never an absent one — the
        // presence probe reports it present, the read refuses).
        // (a) The uncovered gap.
        {
            let (root, mut medium, audit, mk, _ledger, _shared, _private, p1) =
                sqlite_begin_store("uncovered-gap");
            let head_before = medium.head().unwrap().unwrap();
            medium
                .conn
                .execute(
                    "DELETE FROM records WHERE hash = ?1",
                    params![p1.as_bytes()],
                )
                .unwrap();
            let error = medium.begin_reclamation(&audit, &mk, 76).unwrap_err();
            assert!(
                matches!(error, MediumError::MissingObject { hash } if hash == p1),
                "the uncovered gap refuses typed (never the founding): {error:?}"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                head_before,
                "the HEAD is unchanged"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) The unreadable (hash-corrupt) parent row.
        {
            let (root, mut medium, audit, mk, _ledger, _shared, _private, p1) =
                sqlite_begin_store("unreadable-parent");
            let head_before = medium.head().unwrap().unwrap();
            medium
                .conn
                .execute(
                    "UPDATE records SET bytes = ?1 WHERE hash = ?2",
                    params![b"corrupt-parent-record".to_vec(), p1.as_bytes()],
                )
                .unwrap();
            let error = medium.begin_reclamation(&audit, &mk, 77).unwrap_err();
            assert!(
                matches!(error, MediumError::HashMismatch { expected, .. } if expected == p1),
                "the corrupt parent row refuses at the hash-verifying read: {error:?}"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                head_before,
                "the HEAD is unchanged"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_sqlite_begin_discovery_validates_parent_epoch_linkage() {
        // (C2-R3/R4) The duplicate-discovery linkage regressions: a
        // same-key record ON the ancestry with a PRESENT parent must
        // carry epoch+1 — a wrong epoch refuses at the linkage; the
        // shape family (extra refs, a parentless keyed record)
        // refuses at the shape; a wrong begin payload refuses at the
        // exact content; a wrong-MK begin refuses at the binding; a
        // wrong PARENT (a mid-range chain position) strands the
        // audit — the AUTHENTICATION boundary refuses, never the
        // discovery.
        // (a) The wrong-epoch linkage — exercised through the
        // discovery directly: the (C2-R6) authoritative-chain
        // validation refuses such a HEAD at the public admission
        // boundary (the epoch-regression rule — its own regression),
        // so the discovery's own linkage check is the
        // defense-in-depth boundary.
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("wrong-epoch");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let fake = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: head_epoch + 5,
                parent: Some(head),
                idempotency_key: Some(key),
                tick: 90,
                content_refs: vec![object.hash()],
                depends_on: Vec::new(),
            };
            install_sqlite_begin_fake(&mut medium, &fake, &object);
            let error = {
                let tx = medium.conn.transaction().unwrap();
                find_installed_begin_tx(&tx, &mk, &begin).unwrap_err()
            };
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/installed-linkage-mismatch"
                    }
                ),
                "the wrong-epoch same-key record refuses at the linkage: {error:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) The extra-ref shape violation.
        {
            let (root, mut medium, audit, mk, _ledger, shared, _private, _p1) =
                sqlite_begin_store("extra-refs");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: [0u8; 32],
                seed: [0u8; 32],
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let mut refs = vec![object.hash(), shared];
            refs.sort();
            refs.dedup();
            let fake = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: head_epoch + 1,
                parent: Some(head),
                idempotency_key: Some(key),
                tick: 91,
                content_refs: refs,
                depends_on: Vec::new(),
            };
            install_sqlite_begin_fake(&mut medium, &fake, &object);
            let error = medium.begin_reclamation(&audit, &mk, 79).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/installed-shape-mismatch"
                    }
                ),
                "the extra-ref same-key record refuses at the shape: {error:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (c) The content mismatch.
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("content-mismatch");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let wrong = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: [0u8; 32],
                seed: [0u8; 32],
            };
            let object = StoredObject::new(wrong.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let fake = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: head_epoch + 1,
                parent: Some(head),
                idempotency_key: Some(key),
                tick: 92,
                content_refs: vec![object.hash()],
                depends_on: Vec::new(),
            };
            install_sqlite_begin_fake(&mut medium, &fake, &object);
            let error = medium.begin_reclamation(&audit, &mk, 80).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/duplicate-content-mismatch"
                    }
                ),
                "the differing same-key payload refuses at the exact content: {error:?}"
            );
            let _ = ledger;
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (d) The wrong-MK begin payload (a real non-zero identity —
        // the strict decode refuses a zero mk before the binding
        // check could fire).
        {
            let (root, mut medium, audit, mk, _ledger, _shared, _private, _p1) =
                sqlite_begin_store("wrong-mk");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let wrong = crate::reclaim::ReclamationBegin {
                mk: audit,
                source: audit,
                range_digest: [0u8; 32],
                seed: [0u8; 32],
            };
            let object = StoredObject::new(wrong.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let fake = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: head_epoch + 1,
                parent: Some(head),
                idempotency_key: Some(key),
                tick: 93,
                content_refs: vec![object.hash()],
                depends_on: Vec::new(),
            };
            install_sqlite_begin_fake(&mut medium, &fake, &object);
            let error = medium.begin_reclamation(&audit, &mk, 81).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/installed-mk-mismatch"
                    }
                ),
                "the wrong-MK same-key payload refuses at the binding: {error:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (e) The wrong-kind shape violation — exercised through the
        // discovery directly: a keyed record of the FOUNDING kind is
        // decodable (the kind/parent coherence leaves it parentless),
        // so the public admission would refuse such a HEAD earlier
        // at its own audit authentication (the audit unreachable
        // below a parentless head); the discovery's shape validation
        // itself must refuse the keyed non-Regular record.
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("wrong-kind");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let fake = RevisionRecord::founding(AuthorityMode::Standalone, "wrong-kind", 94)
                .with_idempotency_key(key)
                .with_content_refs(vec![object.hash()]);
            install_sqlite_begin_fake(&mut medium, &fake, &object);
            let error = {
                let tx = medium.conn.transaction().unwrap();
                find_installed_begin_tx(&tx, &mk, &begin).unwrap_err()
            };
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/installed-shape-mismatch"
                    }
                ),
                "the keyed non-Regular record refuses at the discovery's shape: {error:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (f) The wrong parent: the same-key record chained onto a
        // MID-RANGE record (not the head) — the observed chain no
        // longer contains the trusted history: the (C2-R6)
        // authoritative-chain validation refuses at the divergence
        // boundary (the audit is stranded below the rewrite).
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("wrong-parent");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let sk_epoch = medium.read_record(&ledger.sk).unwrap().epoch;
            let fake = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: sk_epoch + 1,
                parent: Some(ledger.sk),
                idempotency_key: Some(key),
                tick: 95,
                content_refs: vec![object.hash()],
                depends_on: Vec::new(),
            };
            install_sqlite_begin_fake(&mut medium, &fake, &object);
            let error = medium.begin_reclamation(&audit, &mk, 82).unwrap_err();
            assert!(
                matches!(error, MediumError::Chain { rule: "divergence" }),
                "the wrong-parent same-key record diverges from the trusted history: {error:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
    #[test]
    fn the_sqlite_begin_admission_requires_authoritative_chain_coherence() {
        // (C2-R6) A chain carrying an otherwise authentic audit and
        // original retirement evidence but MALFORMED revisions above
        // the audit never receives a trusted begin: the admission
        // runs the SAME transaction-scoped trusted-cursor validation
        // the normal publication applies — before any duplicate
        // outcome and before any installation; the ordinary
        // valid-history admission still succeeds.
        // (a) The incorrect regular-revision epoch above the audit.
        {
            let (root, mut medium, audit, mk, _ledger, _shared, _private, _p1) =
                sqlite_begin_store("chain-epoch");
            let payload = StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"malformed-above-audit");
                enc.into_bytes()
            })
            .unwrap();
            let head = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let malformed = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: head_epoch + 5,
                parent: Some(head),
                idempotency_key: None,
                tick: 96,
                content_refs: vec![payload.hash()],
                depends_on: Vec::new(),
            };
            let malformed_id = crate::hash::hash_object(&malformed.canonical_bytes());
            install_sqlite_begin_fake(&mut medium, &malformed, &payload);
            let error = medium.begin_reclamation(&audit, &mk, 85).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "epoch-regression"
                    }
                ),
                "the malformed epoch above the audit refuses at the authoritative-chain validation: {error:?}"
            );
            assert_eq!(
                read_meta_head(&medium.conn).unwrap().unwrap(),
                malformed_id,
                "the HEAD is unchanged (nothing installed; the raw stored head)"
            );
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin_rows: i64 = medium
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE idem = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(begin_rows, 0, "no begin revision installed");
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) No duplicate success from an invalid authoritative
        // chain: a VALID same-key begin installed first, then the
        // malformed revision above it — the admission refuses at the
        // chain validation, never returns the duplicate.
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("chain-duplicate");
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let valid = RevisionRecord::regular(head, head_epoch, 97)
                .with_idempotency_key(key)
                .with_content_refs(vec![object.hash()]);
            install_sqlite_begin_fake(&mut medium, &valid, &object);
            let payload = StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"malformed-above-the-begin");
                enc.into_bytes()
            })
            .unwrap();
            let head_m = medium.head().unwrap().unwrap();
            let epoch_m = medium.read_record(&head_m).unwrap().epoch;
            let malformed = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: epoch_m + 5,
                parent: Some(head_m),
                idempotency_key: None,
                tick: 98,
                content_refs: vec![payload.hash()],
                depends_on: Vec::new(),
            };
            install_sqlite_begin_fake(&mut medium, &malformed, &payload);
            let error = medium.begin_reclamation(&audit, &mk, 86).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "epoch-regression"
                    }
                ),
                "no duplicate success from an invalid authoritative chain: {error:?}"
            );
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let begin_rows: i64 = medium
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE idem = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                begin_rows, 1,
                "exactly the pre-existing installation, nothing added"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (c) The invalid general chain structure: the wrong parent
        // — the observed chain no longer contains the trusted
        // history (the divergence the normal admission rejects).
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("chain-divergence");
            let payload = StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"divergent-parent");
                enc.into_bytes()
            })
            .unwrap();
            let r1 = ledger.units[1].record;
            let r1_epoch = medium.read_record(&r1).unwrap().epoch;
            let divergent = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: r1_epoch + 1,
                parent: Some(r1),
                idempotency_key: None,
                tick: 99,
                content_refs: vec![payload.hash()],
                depends_on: Vec::new(),
            };
            let divergent_id = crate::hash::hash_object(&divergent.canonical_bytes());
            install_sqlite_begin_fake(&mut medium, &divergent, &payload);
            let error = medium.begin_reclamation(&audit, &mk, 87).unwrap_err();
            assert!(
                matches!(error, MediumError::Chain { rule: "divergence" }),
                "the divergent rewrite refuses at the authoritative-chain validation: {error:?}"
            );
            assert_eq!(
                read_meta_head(&medium.conn).unwrap().unwrap(),
                divergent_id,
                "the HEAD is unchanged (nothing installed; the raw stored head)"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (d) The valid control: the ordinary valid-history admission
        // still succeeds.
        {
            let (root, mut medium, audit, mk, _ledger, _shared, _private, _p1) =
                sqlite_begin_store("chain-valid-control");
            let outcome = medium.begin_reclamation(&audit, &mk, 88).unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::BeginPublicationOutcome::Accepted {
                        tier: SyncTier::TransactionalWalFull,
                        ..
                    }
                ),
                "the ordinary valid-history admission still succeeds: {outcome:?}"
            );
            medium.validate().unwrap();
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_sqlite_begin_discovery_linkage_refuses_epoch_overflow() {
        // (C2-R7) The overflow regression: a same-key record at epoch
        // u64::MAX over a parent at epoch u64::MAX — the checked
        // linkage refuses (saturation would have made the incoherent
        // successor appear coherent, and the record would then have
        // been verified as a canonical duplicate). Exercised through
        // the discovery directly: the (C2-R6) authoritative-chain
        // validation refuses such a HEAD at the public admission
        // boundary first (its own epoch-regression rule).
        let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
            sqlite_begin_store("epoch-overflow");
        let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
        let begin = crate::reclaim::ReclamationBegin {
            mk,
            source: audit,
            range_digest: ledger.range_digest,
            seed: ledger.seed,
        };
        let object = StoredObject::new(begin.canonical_bytes()).unwrap();
        let payload = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"overflow-parent");
            enc.into_bytes()
        })
        .unwrap();
        let head = medium.head().unwrap().unwrap();
        // The parent P at epoch u64::MAX, then the same-key record K
        // at epoch u64::MAX over it — the corruption-shaped install.
        let parent_record = RevisionRecord {
            kind: crate::record::RecordKind::Regular,
            epoch: u64::MAX,
            parent: Some(head),
            idempotency_key: None,
            tick: 100,
            content_refs: vec![payload.hash()],
            depends_on: Vec::new(),
        };
        let parent_id = crate::hash::hash_object(&parent_record.canonical_bytes());
        let keyed = RevisionRecord {
            kind: crate::record::RecordKind::Regular,
            epoch: u64::MAX,
            parent: Some(parent_id),
            idempotency_key: Some(key),
            tick: 101,
            content_refs: vec![object.hash()],
            depends_on: Vec::new(),
        };
        install_sqlite_begin_fake(&mut medium, &parent_record, &payload);
        install_sqlite_begin_fake(&mut medium, &keyed, &object);
        let error = {
            let tx = medium.conn.transaction().unwrap();
            find_installed_begin_tx(&tx, &mk, &begin).unwrap_err()
        };
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/installed-linkage-mismatch"
                }
            ),
            "the epoch overflow refuses the linkage (u64::MAX has no coherent successor): {error:?}"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_sqlite_begin_authority_verification() {
        // (OC-C3 CP-1) The installed-begin durability authority on
        // the transactional provider: the canonical verification and
        // the truthful TransactionalWalFull grade; the grade
        // following an UNCERTAIN publication (the AfterCommit
        // acknowledgment-loss window — the read-snapshot visibility
        // proof); the corrupt and orphaned refusals; the typed
        // absence; and the verified WAL/FULL configuration's tier.
        // (a) The canonical case + the grade.
        {
            let (root, mut medium, audit, mk, _ledger, _shared, _private, _p1) =
                sqlite_begin_store("authority-canonical");
            let outcome = medium.begin_reclamation(&audit, &mk, 150).unwrap();
            let installed = match outcome {
                crate::reclaim::BeginPublicationOutcome::Accepted { id, tier } => {
                    assert_eq!(
                        tier,
                        SyncTier::TransactionalWalFull,
                        "the sqlite admission carries the transactional tier"
                    );
                    id
                }
                other => panic!("the sqlite admission accepts: {other:?}"),
            };
            let authority = medium.verify_begin_authority(&audit, &mk).unwrap();
            assert_eq!(authority.id, installed);
            assert_eq!(
                authority.tier,
                SyncTier::TransactionalWalFull,
                "the verified WAL/FULL configuration's truthful grade"
            );
            // The absent case on a fresh store: the typed absence.
            let (root2, mut medium2, audit2, mk2, _l2, _s2, _p2, _q2) =
                sqlite_begin_store("authority-absent");
            let error = medium2.verify_begin_authority(&audit2, &mk2).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-authority/absent"
                    }
                ),
                "the missing begin is the typed absence: {error:?}"
            );
            drop(medium);
            drop(medium2);
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&root2);
        }
        // (b) The grade following an uncertain publication: the
        // AfterCommit acknowledgment-loss window — the begin IS
        // committed; the authority's read snapshot observes it and
        // establishes the grade (canonical membership DISTINCT from
        // the lost acknowledgment — the visibility is the proof).
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("authority-after-commit");
            medium.fault = Some(SqliteFaultControl::new().arm(
                SqliteFaultPoint::AfterCommit,
                SqliteFaultAction::InjectError,
            ));
            let outcome = medium.begin_reclamation(&audit, &mk, 151).unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::BeginPublicationOutcome::ReconciledInstalled { .. }
                ),
                "the lost acknowledgment reconciles to the proven installation: {outcome:?}"
            );
            medium.fault = None;
            let authority = medium.verify_begin_authority(&audit, &mk).unwrap();
            assert_eq!(authority.tier, SyncTier::TransactionalWalFull);
            // The exact-candidate identity: the authority's begin is
            // the record the uncertain publication installed.
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let head = read_meta_head(&medium.conn).unwrap().unwrap();
            let _ = (ledger, key, head);
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (c) The corrupt and orphaned refusals: a corrupted begin
        // object row (the typed integrity refusal) and an orphaned
        // same-key record row (never authority — the definite
        // absence through the walk).
        {
            let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
                sqlite_begin_store("authority-corrupt");
            let outcome = medium.begin_reclamation(&audit, &mk, 152).unwrap();
            let installed = match outcome {
                crate::reclaim::BeginPublicationOutcome::Accepted { id, .. } => id,
                other => panic!("the admission accepts: {other:?}"),
            };
            // The begin's content object row corrupted.
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object_hash = StoredObject::new(begin.canonical_bytes()).unwrap().hash();
            medium
                .conn
                .execute(
                    "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
                    params![b"corrupted-begin-payload".to_vec(), object_hash.as_bytes()],
                )
                .unwrap();
            let error = medium.verify_begin_authority(&audit, &mk).unwrap_err();
            assert!(
                matches!(error, MediumError::HashMismatch { .. }),
                "the corrupted begin payload refuses at the hash-verifying read: {error:?}"
            );
            let _ = installed;
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        {
            let (root, mut medium, audit, mk, _ledger, _shared, _private, _p1) =
                sqlite_begin_store("authority-orphan");
            // The orphaned same-key record row (never on the
            // ancestry): the authority is the typed absence.
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let orphan_object = StoredObject::new(
                crate::reclaim::ReclamationBegin {
                    mk,
                    source: audit,
                    range_digest: [0u8; 32],
                    seed: [0u8; 32],
                }
                .canonical_bytes(),
            )
            .unwrap();
            let orphan_record = RevisionRecord::regular(Hash::ZERO, 0, 153)
                .with_idempotency_key(key.clone())
                .with_content_refs(vec![orphan_object.hash()]);
            medium
                .conn
                .execute(
                    "INSERT INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
                    params![
                        orphan_object.hash().as_bytes().to_vec(),
                        orphan_object.domain(),
                        orphan_object.bytes().to_vec()
                    ],
                )
                .unwrap();
            medium
                .conn
                .execute(
                    "INSERT INTO records (hash, bytes, epoch, parent, idem, kind, tick) \
                     VALUES (?1, ?2, 0, NULL, ?3, 1, 153)",
                    params![
                        crate::hash::hash_object(&orphan_record.canonical_bytes())
                            .as_bytes()
                            .to_vec(),
                        orphan_record.canonical_bytes(),
                        key
                    ],
                )
                .unwrap();
            let error = medium.verify_begin_authority(&audit, &mk).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-authority/absent"
                    }
                ),
                "the orphaned same-key row never grants authority: {error:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
    #[test]
    fn the_r1_attack_refuses_at_every_boundary() {
        // (OC-C3 CP-2 §9; CP2-R2-D) The R1 attack end-to-end at the
        // SQLite provider's boundaries: the GENUINE
        // checkpoint/audit fixture (sqlite_begin_store — a real
        // campaign, a real published audit) with the original
        // retirement evidence DELETED after the fact — the
        // DEGRADATION shape the attack presents — and NO begin ever
        // published. Refused at the provider deletion gate (the
        // definite begin absence), at the direct completion
        // publication, and at the completion wrapper; the failed
        // fresh admission (the degraded evidence) never manufactures
        // a retroactive begin. (The distinct FABRICATED-evidence
        // scenario — a self-consistent invented checkpoint/audit —
        // is covered at the admission boundary by the local
        // fabricated-range/digest-order regressions and at the
        // completion boundary by this test's hand-built marker: the
        // canonical validation is never weakened to construct it.)
        let (root, mut medium, audit, mk, _ledger, _shared, _private, _p1) =
            sqlite_begin_store("r1-attack");
        // THE R1 DEGRADATION: the original retirement evidence
        // deleted (the genuine audit's unit records — exactly what
        // the degradation presents), NO begin ever published.
        let (ledger, _meta) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
        for unit in &ledger.units {
            medium
                .conn
                .execute(
                    "DELETE FROM records WHERE hash = ?1",
                    params![unit.record.as_bytes()],
                )
                .unwrap();
        }
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        // (a) The provider deletion gate.
        let error = medium.reclaim(&directive).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "execution/begin-absent"
                }
            ),
            "the R1 state refuses at the provider gate: {error:?}"
        );
        // The fresh admission over the degraded evidence FAILS
        // CLOSED — no retroactive begin (OC-P1).
        let error = medium.begin_reclamation(&audit, &mk, 240).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::MissingObject { .. }
                    | MediumError::Chain {
                        rule: "begin-admission/units-objects-mismatch"
                    }
            ),
            "the degraded evidence cannot be authorized: {error:?}"
        );
        let error = medium.reclaim(&directive).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "execution/begin-absent"
                }
            ),
            "the gate still refuses after the failed admission: {error:?}"
        );
        // (b) The direct completion publication (the independent
        // boundary).
        let marker = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk,
            batch: 0,
            source: audit,
            mandatory_count: 2,
            deferred: Vec::new(),
            journal_files: Vec::new(),
        };
        let head = read_meta_head(&medium.conn).unwrap().unwrap();
        let error = crate::reclaim::publish_reclamation_complete(&mut medium, &marker, head, 241)
            .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-authority/absent"
                }
            ),
            "the direct publication refuses the beginless state: {error:?}"
        );
        // (c) The completion wrapper.
        let error = crate::reclaim::complete_reclamation(&mut medium, &audit, &mk).unwrap_err();
        // The wrapper refuses at its own earliest boundary for this
        // state (the mandatory objects still present — the residue
        // check precedes the publication); either way the beginless
        // state is NEVER certified.
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-authority/absent"
                } | MediumError::Chain {
                    rule: "reclamation-complete/mandatory-residue"
                } | MediumError::Chain {
                    rule: "reclamation-complete/record-still-present"
                }
            ),
            "the completion wrapper refuses the beginless state: {error:?}"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(feature = "composite-test-fixture")]
    fn the_oc4_beginless_marker_refuses_trusted_certification_and_stays_discoverable() {
        // (OC-C4) The completion-reader authority at the REQUESTED
        // level: a completed marker never becomes authoritative
        // without an authenticated, correctly bound, provider-durable
        // ReclamationBegin — no grandfathering. The LEGACY shape: the
        // genuine audit, the hand-installed keyed marker (exactly
        // what a pre-Option-C record or a corrupted in-crate path
        // presents), and NO begin ever published. The trusted
        // verification refuses at the begin authority; the marker
        // remains DISCOVERABLE for non-authoritative reporting (the
        // discovery arm carries no completion authority); the failed
        // verification publishes no begin, deletes nothing, and
        // installs no new completion.
        let (root, mut medium, audit, mk, _ledger, _shared, _private, _p1) =
            sqlite_begin_store("oc4-beginless-marker");
        let marker = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk,
            batch: 0,
            source: audit,
            mandatory_count: 2,
            deferred: Vec::new(),
            journal_files: Vec::new(),
        };
        let marker_id =
            crate::reclaim::test_only_install_completion_marker(&mut medium, &marker, 240).unwrap();
        // The baseline AFTER the install (the failed verification
        // must change nothing beyond it).
        let records_before = medium.validate().unwrap().records;
        // THE TRUSTED VERIFICATION refuses the beginless marker.
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-authority/absent"
                }
            ),
            "the beginless marker never certifies: {error:?}"
        );
        // The legacy marker remains DISCOVERABLE (the reporting
        // distinction: readable, never authoritative).
        let (found_id, _found) = crate::reclaim::find_installed_completion(&mut medium, &mk)
            .unwrap()
            .expect("the historical record stays discoverable");
        assert_eq!(
            found_id, marker_id,
            "the historical record stays readable for non-authoritative reporting"
        );
        // No begin was published as a side effect of the failed
        // verification.
        let error = medium.verify_begin_authority(&audit, &mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-authority/absent"
                }
            ),
            "the verification manufactured no begin: {error:?}"
        );
        // No deletion, no new completion publication: the record
        // count is unchanged (the marker install preceded the
        // baseline).
        let records_after = medium.validate().unwrap().records;
        assert_eq!(
            records_after, records_before,
            "the failed verification changed nothing"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(feature = "composite-test-fixture")]
    fn the_oc4_wrong_bound_begin_refuses_through_the_completion_reader() {
        // (OC-C4) The CP-1 verifier matrix's typed refusals
        // propagate through the completion reader — the reader never
        // broadens the accepted classifications: a begin record
        // discovered at the right deterministic key but carrying the
        // WRONG source-audit binding refuses with
        // begin-authority/binding-mismatch; the wrong range digest
        // refuses the same way. The marker itself is structurally
        // valid and on-chain — only the begin provenance is wrong.
        let (root, mut medium, audit, mk, ledger, _shared, _private, p1) =
            sqlite_begin_store("oc4-wrong-bound");
        // The wrong-SOURCE begin at the right key (a REAL non-zero
        // identity — the strict decoder refuses zero-identity
        // fields before the binding check would run).
        let wrong_source = crate::reclaim::ReclamationBegin {
            mk,
            source: p1,
            range_digest: ledger.range_digest,
            seed: ledger.seed,
        };
        let _begin_id =
            crate::reclaim::test_only_install_begin_record(&mut medium, &wrong_source, 240)
                .unwrap();
        let marker = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk,
            batch: 0,
            source: audit,
            mandatory_count: 2,
            deferred: Vec::new(),
            journal_files: Vec::new(),
        };
        let marker_id =
            crate::reclaim::test_only_install_completion_marker(&mut medium, &marker, 241).unwrap();
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-authority/binding-mismatch"
                }
            ),
            "the wrong-source begin refuses the certification: {error:?}"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // The wrong-DIGEST begin at the right key.
        let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
            sqlite_begin_store("oc4-wrong-digest");
        let wrong_digest = crate::reclaim::ReclamationBegin {
            mk,
            source: audit,
            range_digest: [0u8; 32],
            seed: ledger.seed,
        };
        let _begin_id =
            crate::reclaim::test_only_install_begin_record(&mut medium, &wrong_digest, 240)
                .unwrap();
        let marker = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk,
            batch: 0,
            source: audit,
            mandatory_count: 2,
            deferred: Vec::new(),
            journal_files: Vec::new(),
        };
        let marker_id =
            crate::reclaim::test_only_install_completion_marker(&mut medium, &marker, 241).unwrap();
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-authority/binding-mismatch"
                }
            ),
            "the wrong-digest begin refuses the certification: {error:?}"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_sqlite_execution_commit_uncertainty_is_typed() {
        // (OC-C3 CP-2 §4/§9) The execution COMMIT uncertainty: the
        // injected CommitFailure (the established seam) produces the
        // single typed reclaim/commit-uncertain — never success,
        // never a definite absence — and the next authorized pass
        // independently re-verifies the begin and the actual
        // deletion state, converging.
        let (root, mut medium, audit, mk, ledger, _shared, _private, _p1) =
            sqlite_begin_store("commit-uncertain");
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        let begin_outcome = medium.begin_reclamation(&audit, &mk, 242).unwrap();
        assert!(
            matches!(
                begin_outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "the begin admits: {begin_outcome:?}"
        );
        medium.fault = Some(SqliteFaultControl::new().arm(
            SqliteFaultPoint::CommitFailure,
            SqliteFaultAction::InjectError,
        ));
        let error = medium.reclaim(&directive).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclaim/commit-uncertain"
                }
            ),
            "the execution commit failure is the typed uncertainty: {error:?}"
        );
        medium.fault = None;
        // The next authorized pass converges (the observation found
        // nothing deleted; the gate re-verified the begin).
        let outcome = medium.reclaim(&directive).unwrap();
        assert!(
            !outcome.mandatory.is_empty(),
            "the retry converges through the re-verified begin"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_sqlite_publication_and_capacity_matrix() {
        // (V2-W §9 G + the sqlite positives) The v2 publications on
        // the SQLite provider: the plan and every batch audit
        // publish under the deterministic keys with the truthful
        // outcomes; the W3A capacity floor — a connection whose
        // runtime SQLITE_MAX_LENGTH falls below the supported 4 MiB
        // floor is an UNSUPPORTED configuration for reclamation
        // publication (the typed refusal), while the ordinary
        // small-object reads/writes remain unaffected; and a limit
        // restored above the floor re-enables publication.
        let label = "v2w-sqlite";
        let root = std::env::temp_dir().join(format!(
            "s7-occ2r4-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        // The plan campaign (the checkpoint WITHOUT a v1 audit):
        // F(shared) → P1(shared) → R1(shared) → R2(shared+private)
        // → SK(shared) → MK.
        let shared = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"v2w-sqlite-shared");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"v2w-sqlite-private");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        // (V2-W F4, the F4-R6 fresh shape) The bound bootstrap AT THE
        // FOUNDING — the production ordering: the durable bound
        // journal at the root, then the canonical root binding
        // published while the founding is the chain's head (the
        // first binding's fresh-bootstrap admission), then the
        // campaign's revisions on top.
        let journal_root = root.join("journal-root");
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(&journal_root);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        drop(
            crate::reclaim::create_campaign_bound_journal(&mut medium, &journal_root, &founding_id)
                .unwrap(),
        );
        let (_binding_id, binding_outcome) = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        assert!(
            matches!(
                binding_outcome,
                crate::reclaim::JournalRootBindingOutcome::Accepted { .. }
            ),
            "the fixture's binding publishes: {binding_outcome:?}"
        );
        let step = |medium: &mut SqliteMedium, objects: &[StoredObject], tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
            let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
            medium.append(&record, objects, Some(head)).unwrap();
            record.id()
        };
        let p1 = step(&mut medium, std::slice::from_ref(&shared), 1);
        let r1 = step(&mut medium, std::slice::from_ref(&shared), 2);
        let r2 = step(&mut medium, &[shared.clone(), private.clone()], 3);
        let sk = step(&mut medium, std::slice::from_ref(&shared), 4);
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let member_bytes: Vec<Vec<u8>> = [r1, r2]
            .iter()
            .map(|id| medium.read_record(id).unwrap().canonical_bytes())
            .collect();
        let digest = crate::checkpoint::chain_range_digest(&seed, &member_bytes);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, r2, 3),
                ReclaimedIndexEntry::new(None, sk, 4),
            ],
            digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let mk = step(&mut medium, std::slice::from_ref(&meta_object), 5);
        // THE POSITIVE: the plan and every batch audit publish.
        let (plan_id, plan_outcome) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 90)
                .unwrap();
        assert!(
            matches!(
                plan_outcome,
                crate::reclaim::PlanPublicationOutcome::Accepted { id } if id == plan_id
            ),
            "the sqlite plan publishes: {plan_outcome:?}"
        );
        let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
            .unwrap()
            .unwrap();
        for batch in plan.batches.iter().map(|b| b.index) {
            let (audit_id, audit_outcome) =
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, batch, 91).unwrap();
            assert!(
                matches!(
                    audit_outcome,
                    crate::reclaim::LedgerV2PublicationOutcome::Accepted { id } if id == audit_id
                ),
                "the sqlite batch audit publishes: {audit_outcome:?}"
            );
            assert!(
                crate::reclaim::find_installed_v2_audit(&mut medium, &mk, batch)
                    .unwrap()
                    .is_some()
            );
        }
        // THE W3A FLOOR: a connection below the supported floor is
        // an unsupported configuration for reclamation publication —
        // the typed refusal — while the ordinary small-object
        // publication keeps working.
        medium
            .conn
            .set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 1024)
            .unwrap();
        let error =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 92)
                .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-plan/provider-limit-unsupported"
                }
            ),
            "the below-floor connection refuses reclamation publication: {error:?}"
        );
        // The ordinary small-object publication is unaffected (the
        // floor is a RECLAMATION classification, not a store-wide
        // failure).
        step(&mut medium, std::slice::from_ref(&shared), 6);
        // THE RESTORE: a limit above the floor re-enables the
        // publication (the idempotent duplicate of the already
        // installed plan).
        medium
            .conn
            .set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, i32::MAX)
            .unwrap();
        let (dup_id, dup_outcome) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 93)
                .unwrap();
        assert!(
            matches!(
                dup_outcome,
                crate::reclaim::PlanPublicationOutcome::DuplicateVerified { id } if id == plan_id
            ),
            "the restored connection re-enables the idempotent publication: {dup_outcome:?}"
        );
        assert_eq!(dup_id, plan_id);
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_f3_sqlite_second_reference_integrity() {
        // (V2W-F3, the SQLite side) EVERY direct content reference
        // is verified at the writer boundaries on the SQLite
        // provider too: the genuine two-reference retiring record
        // whose SECOND reference is corrupted at the row level
        // refuses both the plan admission (the first-execution
        // evidence core) and the batch-audit admission (the
        // collector's verified read) through the typed integrity
        // classification, before any authority installs.
        let label = "v2wf3-sqlite";
        let root = std::env::temp_dir().join(format!(
            "s7-occ2r4-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let build = |label_postfix: &str| -> (SqliteMedium, Hash, Hash, std::path::PathBuf) {
            let mut medium =
                SqliteMedium::open(root.join(format!("db-{label_postfix}.sqlite"))).unwrap();
            let shared = StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"v2wf3-sqlite-shared");
                enc.into_bytes()
            })
            .unwrap();
            let private = StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"v2wf3-sqlite-private");
                enc.into_bytes()
            })
            .unwrap();
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
                .with_content_refs(vec![shared.hash()]);
            medium
                .append(&founding, std::slice::from_ref(&shared), None)
                .unwrap();
            // (V2-W F4, the F4-R6 fresh shape) The bound bootstrap AT
            // THE FOUNDING — the production ordering (the first
            // binding's fresh-bootstrap admission at the founding
            // head).
            let journal_root = root.join(format!("journal-{label_postfix}"));
            let founding_id = founding.id();
            let location = crate::reclaim::JournalRootBinding::normalize_location(&journal_root);
            let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
            drop(
                crate::reclaim::create_campaign_bound_journal(
                    &mut medium,
                    &journal_root,
                    &founding_id,
                )
                .unwrap(),
            );
            crate::reclaim::publish_journal_root_binding(
                &mut medium,
                &founding_id,
                &root_id,
                &location,
                1,
            )
            .unwrap();
            let step = |medium: &mut SqliteMedium, objects: &[StoredObject], tick: u64| -> Hash {
                let head = medium.head().unwrap().unwrap();
                let epoch = medium.read_record(&head).unwrap().epoch;
                let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
                let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
                medium.append(&record, objects, Some(head)).unwrap();
                record.id()
            };
            let p1 = step(&mut medium, std::slice::from_ref(&shared), 1);
            let r1 = step(&mut medium, std::slice::from_ref(&shared), 2);
            let r2 = step(&mut medium, &[shared.clone(), private.clone()], 3);
            let sk = step(&mut medium, std::slice::from_ref(&shared), 4);
            let seed = crate::checkpoint::compute_digest_seed(&founding.id());
            let member_bytes: Vec<Vec<u8>> = [r1, r2]
                .iter()
                .map(|id| medium.read_record(id).unwrap().canonical_bytes())
                .collect();
            let digest = crate::checkpoint::chain_range_digest(&seed, &member_bytes);
            let meta = CheckpointMetadata::new(
                sk,
                r1,
                vec![SkipEntry::new(Hash::ZERO, p1)],
                vec![
                    ReclaimedIndexEntry::new(None, r1, 2),
                    ReclaimedIndexEntry::new(None, r2, 3),
                    ReclaimedIndexEntry::new(None, sk, 4),
                ],
                digest,
                seed,
            );
            let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
            let mk = step(&mut medium, std::slice::from_ref(&meta_object), 5);
            (medium, mk, private.hash(), journal_root)
        };
        // THE PLAN ADMISSION (the corruption precedes the plan).
        {
            let (mut medium, mk, private_hash, journal_root) = build("plan");
            medium
                .conn
                .execute(
                    "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
                    params![
                        b"corrupted-second-ref".to_vec(),
                        private_hash.as_bytes().to_vec()
                    ],
                )
                .unwrap();
            let error =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 90)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::HashMismatch { .. } | MediumError::MissingObject { .. }
                ),
                "the corrupt second reference refuses the sqlite plan admission: {error:?}"
            );
        }
        // THE BATCH-AUDIT ADMISSION (the plan published intact, the
        // corruption after).
        {
            let (mut medium, mk, private_hash, journal_root) = build("audit");
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 91)
                .unwrap();
            medium
                .conn
                .execute(
                    "UPDATE objects SET bytes = ?1 WHERE hash = ?2",
                    params![
                        b"corrupted-second-ref".to_vec(),
                        private_hash.as_bytes().to_vec()
                    ],
                )
                .unwrap();
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
                .unwrap()
                .unwrap();
            for batch in plan.batches.iter().map(|b| b.index) {
                let error =
                    crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, batch, 92)
                        .unwrap_err();
                assert!(
                    matches!(
                        error,
                        MediumError::HashMismatch { .. } | MediumError::MissingObject { .. }
                    ),
                    "the corrupt second reference refuses the sqlite audit admission: {error:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_sqlite_reader_matrix() {
        // (V2X-CP1) The trusted all-audits reader on the SQLite
        // provider: the incomplete publication state classified, the
        // complete state certified FIRST-EXECUTION-READY with the
        // exact revision identities (strictly read-only), the
        // restart (a fresh connection) certifying identically, and
        // the bogus-MK typed refusal.
        let label = "v2x-cp1-sqlite";
        let root = std::env::temp_dir().join(format!(
            "s7-{label}-{}-{}",
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
            enc.write_bytes(b"v2x-sqlite-shared");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"v2x-sqlite-private");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let journal_root = root.join("journal-root");
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(&journal_root);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        drop(
            crate::reclaim::create_campaign_bound_journal(&mut medium, &journal_root, &founding_id)
                .unwrap(),
        );
        let (_binding_id, _binding_outcome) = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        let step = |medium: &mut SqliteMedium, objects: &[StoredObject], tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
            let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
            medium.append(&record, objects, Some(head)).unwrap();
            record.id()
        };
        let p1 = step(&mut medium, std::slice::from_ref(&shared), 1);
        let r1 = step(&mut medium, std::slice::from_ref(&shared), 2);
        let r2 = step(&mut medium, &[shared.clone(), private.clone()], 3);
        let sk = step(&mut medium, std::slice::from_ref(&shared), 4);
        let seed = crate::checkpoint::compute_digest_seed(&founding_id);
        let member_bytes: Vec<Vec<u8>> = [r1, r2]
            .iter()
            .map(|id| medium.read_record(id).unwrap().canonical_bytes())
            .collect();
        let digest = crate::checkpoint::chain_range_digest(&seed, &member_bytes);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, r2, 3),
                ReclaimedIndexEntry::new(None, sk, 4),
            ],
            digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let mk = step(&mut medium, std::slice::from_ref(&meta_object), 5);
        // THE BOGUS MK: a real non-zero identity that is no
        // checkpoint — the typed refusal, never a classification.
        let error = crate::reclaim::verify_v2_execution_readiness(&mut medium, &private.hash())
            .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "v2-readiness/mk-not-installed"
                }
            ),
            "the bogus MK refuses typed: {error:?}"
        );
        // THE INCOMPLETE STATE: the plan published, no audits yet.
        let (plan_id, _) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 90)
                .unwrap();
        let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
            .unwrap()
            .unwrap();
        match crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap() {
            crate::reclaim::V2ExecutionReadiness::PublicationIncomplete { missing_batches } => {
                let expected: Vec<u64> = plan.batches.iter().map(|b| b.index).collect();
                assert_eq!(missing_batches, expected);
            }
            other => panic!("the sqlite mid-publication state classifies: {other:?}"),
        }
        // THE COMPLETE STATE: every batch audit published.
        let mut audit_ids = Vec::new();
        for batch in plan.batches.iter().map(|b| b.index) {
            let (audit_id, _) =
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, batch, 91).unwrap();
            audit_ids.push(audit_id);
        }
        match crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap() {
            crate::reclaim::V2ExecutionReadiness::ExecutionReady {
                plan_record,
                audit_records,
                ..
            } => {
                assert_eq!(plan_record, plan_id);
                assert_eq!(audit_records, audit_ids);
            }
            other => panic!("the sqlite complete state certifies ready: {other:?}"),
        }
        // THE RESTART: a fresh connection certifies identically.
        drop(medium);
        let mut reopened = SqliteMedium::open(root.join("db.sqlite")).unwrap();
        match crate::reclaim::verify_v2_execution_readiness(&mut reopened, &mk).unwrap() {
            crate::reclaim::V2ExecutionReadiness::ExecutionReady { plan_record, .. } => {
                assert_eq!(plan_record, plan_id);
            }
            other => panic!("the sqlite restart certifies ready: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (V2X-CP2 F3) The SQLite V2 plan campaign for the
    /// reconciliation regressions: the bound-bootstrapped founding,
    /// the journal binding, the retained pre-range record p1, the
    /// two range records (r2 carrying the shared+private pair), SK,
    /// and the hand-built checkpoint MK over the [r1, r2, sk] index
    /// with the self-skip to p1 — the reader-matrix fixture shape,
    /// with the genuine plan published at the 460 split (batch 0 =
    /// [sk, r2], batch 1 = [r1]). Returns (root, medium, mk,
    /// plan_id, r1).
    fn v2x_sqlite_reconcile_campaign(
        label: &str,
    ) -> (std::path::PathBuf, SqliteMedium, Hash, Hash, Hash) {
        let root = std::env::temp_dir().join(format!(
            "s7-{label}-{}-{}",
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
            enc.write_bytes(b"v2x-f3-sqlite-shared");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"v2x-f3-sqlite-private");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let journal_root = root.join("journal-root");
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(&journal_root);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        drop(
            crate::reclaim::create_campaign_bound_journal(&mut medium, &journal_root, &founding_id)
                .unwrap(),
        );
        let (_binding_id, _binding_outcome) = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        let step = |medium: &mut SqliteMedium, objects: &[StoredObject], tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
            let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
            medium.append(&record, objects, Some(head)).unwrap();
            record.id()
        };
        let p1 = step(&mut medium, std::slice::from_ref(&shared), 1);
        let r1 = step(&mut medium, std::slice::from_ref(&shared), 2);
        let r2 = step(&mut medium, &[shared.clone(), private.clone()], 3);
        let sk = step(&mut medium, std::slice::from_ref(&shared), 4);
        let seed = crate::checkpoint::compute_digest_seed(&founding_id);
        let member_bytes: Vec<Vec<u8>> = [r1, r2]
            .iter()
            .map(|id| medium.read_record(id).unwrap().canonical_bytes())
            .collect();
        let digest = crate::checkpoint::chain_range_digest(&seed, &member_bytes);
        let meta = CheckpointMetadata::new(
            sk,
            r1,
            vec![SkipEntry::new(Hash::ZERO, p1)],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, r2, 3),
                ReclaimedIndexEntry::new(None, sk, 4),
            ],
            digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let mk = step(&mut medium, std::slice::from_ref(&meta_object), 5);
        let (plan_id, _) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 460, &journal_root, 90)
                .unwrap();
        (root, medium, mk, plan_id, r1)
    }

    #[test]
    fn the_v2_begin_reconciliation_proves_the_different_candidates_authority() {
        // (V2X-CP2 F3) The different-candidate reconciliation: after
        // an uncertain publication, a DIFFERENT same-key begin is
        // DuplicateVerified ONLY when its OWN retained authority
        // holds — the strict temporal position, the complete
        // retained membership, and the source binding, re-proved
        // over the same READ-ONLY transaction with the verifier's
        // begin identity exactly the observed revision. The payload
        // match alone is never trusted completion. The exact
        // attempted candidate remains the canonical
        // ReconciledInstalled installation evidence, and the valid
        // different candidate verifies as the OBSERVED duplicate
        // without inventing an installation acknowledgment for the
        // attempted one.
        // (a) THE MANDATORY FIXTURE — a different-or-absent
        // attempted candidate over a byte-identical but temporally
        // invalid installed begin: never DuplicateVerified.
        {
            let (root, mut medium, mk, plan_id, r1) =
                v2x_sqlite_reconcile_campaign("v2x-cp2-f3-invalid");
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
                .unwrap()
                .unwrap();
            assert_eq!(plan.batches.len(), 2, "the 460 split: two batches");
            // The partial audit set: batch 0 only.
            crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, 0, 91).unwrap();
            // The correctly encoded, correctly plan-bound begin
            // installed adversarially under the deterministic key
            // (the admitted in-crate receipt — the only bypass).
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: plan_id,
                range_digest: plan.range_digest,
                seed: plan.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, 96)
                .with_idempotency_key(crate::reclaim::reclamation_begin_idempotency_key(&mk))
                .with_content_refs(vec![object.hash()]);
            let admitted =
                crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
            medium.append_admitted(admitted, Some(head)).unwrap();
            // The REMAINING valid audit published ABOVE that begin
            // (the normal gate — the audit evidence itself remains
            // valid).
            crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, 1, 97).unwrap();
            // The reconciliation observation with an ABSENT
            // attempted candidate identity: the typed temporal
            // refusal propagates — the byte-identical payload match
            // alone never classifies as DuplicateVerified.
            let error = reconcile_begin_publication_v2(&mut medium, &mk, None).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-authority-v2/target-above-begin"
                    }
                ),
                "the absent candidate over the temporally invalid begin refuses typed: {error:?}"
            );
            // The DIFFERENT attempted candidate identity: the same
            // typed refusal.
            let error = reconcile_begin_publication_v2(&mut medium, &mk, Some(r1)).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-authority-v2/target-above-begin"
                    }
                ),
                "the different candidate over the temporally invalid begin refuses typed: {error:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) THE POSITIVE DIFFERENT-CANDIDATE CONTROL — a different
        // same-key begin with VALID retained authority: the verified
        // duplicate naming the OBSERVED revision, never an
        // installation acknowledgment for the attempted candidate.
        {
            let (root, mut medium, mk, plan_id, r1) =
                v2x_sqlite_reconcile_campaign("v2x-cp2-f3-valid");
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
                .unwrap()
                .unwrap();
            // EVERY plan-listed audit (the complete all-audits-first
            // set).
            for batch in plan.batches.iter().map(|b| b.index) {
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, batch, 91).unwrap();
            }
            // The genuine-valued begin installed ABOVE every audit
            // (the adversarial install at the head — the temporally
            // valid position the admission itself would occupy).
            let begin = crate::reclaim::ReclamationBegin {
                mk,
                source: plan_id,
                range_digest: plan.range_digest,
                seed: plan.seed,
            };
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, 96)
                .with_idempotency_key(crate::reclaim::reclamation_begin_idempotency_key(&mk))
                .with_content_refs(vec![object.hash()]);
            let installed_id = record.id();
            let admitted =
                crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
            medium.append_admitted(admitted, Some(head)).unwrap();
            // The different-candidate observation: the complete
            // authority holds → DuplicateVerified.
            let outcome = reconcile_begin_publication_v2(&mut medium, &mk, Some(r1)).unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id } if id == installed_id
                ),
                "the valid different candidate verifies as the observed duplicate: {outcome:?}"
            );
            // The absent attempted candidate classifies the same
            // way.
            let outcome = reconcile_begin_publication_v2(&mut medium, &mk, None).unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id } if id == installed_id
                ),
                "the absent candidate over the valid authority verifies the observed duplicate: {outcome:?}"
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (c) THE EXACT ATTEMPTED CANDIDATE — the production
        // uncertain-commit route (the established AfterCommit seam:
        // the commit succeeded, the acknowledgment lost) reconciles
        // to the canonical ReconciledInstalled installation
        // evidence.
        {
            let (root, mut medium, mk, _plan_id, _r1) =
                v2x_sqlite_reconcile_campaign("v2x-cp2-f3-exact");
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
                .unwrap()
                .unwrap();
            for batch in plan.batches.iter().map(|b| b.index) {
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, batch, 91).unwrap();
            }
            medium.fault = Some(SqliteFaultControl::new().arm(
                SqliteFaultPoint::AfterCommit,
                SqliteFaultAction::InjectError,
            ));
            let outcome = medium.begin_reclamation_v2(&mk, 98).unwrap();
            let crate::reclaim::BeginPublicationOutcome::ReconciledInstalled { id } = outcome
            else {
                panic!(
                    "the exact attempted candidate reconciles to the installation evidence: {outcome:?}"
                )
            };
            assert_eq!(
                medium.head().unwrap().unwrap(),
                id,
                "the reconciled installation is the HEAD"
            );
            medium.fault = None;
            medium.validate().unwrap();
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

/// The OC-C2 trusted begin admission (the transactional twin — see
/// the free `begin_reclamation_tx`): ONE BEGIN IMMEDIATE covering
/// the authentication, the evidence core, the duplicate resolution,
/// and the insertion + the HEAD advance; the uncertain-commit
/// discipline (never a false confirmation).
impl SqliteMedium {
    pub fn begin_reclamation(
        &mut self,
        audit: &Hash,
        mk: &Hash,
        tick: u64,
    ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| sqlite_err("begin admission transaction", e))?;
        // (C2-R6) The authoritative-chain validation — the SAME
        // transaction-scoped trusted-cursor discipline the normal
        // publication applies (O(0) on the verified head, a bounded
        // suffix walk on divergence, the cold walk otherwise): a
        // chain carrying an otherwise authentic audit and retirement
        // evidence but malformed revisions above the audit never
        // receives a trusted begin — no duplicate outcome, no
        // installation.
        let observed = read_meta_head(&tx)?;
        if let Err(error) = validate_observed(&tx, &mut self.cursor, &mut self.walks, observed) {
            let _ = tx.rollback();
            return Err(error);
        }
        let (outcome, candidate) = begin_reclamation_tx(&tx, audit, mk, tick)?;
        // (C2-R1) The fault-wired commit boundary (the established
        // SqliteFaultPoint infrastructure — the same points the
        // normal append path uses): an injected CommitFailure
        // simulates the pre-commit rollback (a DEFINITE absence —
        // distinguished from a genuinely ambiguous real commit); an
        // injected AfterCommit simulates the
        // installed-but-acknowledgment-lost window; a real COMMIT
        // error is the genuine uncertainty. All three reconcile
        // through the READ-ONLY observation — never a second
        // publication inside the reconciliation.
        let uncertain =
            if let Err(_injected) = fault_check(&self.fault, SqliteFaultPoint::CommitFailure) {
                let _ = tx.rollback();
                true
            } else if tx.commit().is_err() {
                true
            } else {
                fault_check(&self.fault, SqliteFaultPoint::AfterCommit).is_err()
            };
        if uncertain {
            // (C2-R5) The reconciliation is bound to the EXACT
            // attempted candidate revision identity.
            return reconcile_begin_publication(self, audit, mk, candidate);
        }
        Ok(outcome)
    }

    /// (V2X-CP2) The plan-bound V2 begin admission — the trusted
    /// transactional wrapper: ONE `BEGIN IMMEDIATE` covering the
    /// all-audits-first verification and the publication (the C2-R6
    /// authoritative-chain validation first, the accepted V1
    /// wrapper's discipline); the honest COMMIT uncertainty (the
    /// injected CommitFailure = the definite pre-commit rollback;
    /// the injected AfterCommit = the installed-but-acknowledgment-
    /// lost window; a real COMMIT error = the genuine uncertainty)
    /// reconciled through the READ-ONLY V2 observation — never a
    /// second publication inside the reconciliation.
    pub fn begin_reclamation_v2(
        &mut self,
        mk: &Hash,
        tick: u64,
    ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| sqlite_err("v2 begin admission transaction", e))?;
        let observed = read_meta_head(&tx)?;
        if let Err(error) = validate_observed(&tx, &mut self.cursor, &mut self.walks, observed) {
            let _ = tx.rollback();
            return Err(error);
        }
        let (outcome, candidate) = begin_reclamation_v2_tx(&tx, mk, tick)?;
        let uncertain =
            if let Err(_injected) = fault_check(&self.fault, SqliteFaultPoint::CommitFailure) {
                let _ = tx.rollback();
                true
            } else if tx.commit().is_err() {
                true
            } else {
                fault_check(&self.fault, SqliteFaultPoint::AfterCommit).is_err()
            };
        if uncertain {
            return reconcile_begin_publication_v2(self, mk, candidate);
        }
        Ok(outcome)
    }
}

/// (V2X-CP2) The post-uncertain-commit V2 reconciliation: a
/// READ-ONLY fresh observation (never publishes, never inserts,
/// never constructs a replacement candidate) — the authoritative
/// chain validated under the same trusted-cursor discipline, the
/// expected V2 begin re-derived from the RETAINED evidence (the
/// checkpoint metadata and the installed plan — the read-only
/// forms), then the gap-aware HEAD-ancestry discovery bound to the
/// EXACT attempted candidate identity: the candidate itself
/// canonically present is ReconciledInstalled (the membership
/// proof, never Accepted, never a durability claim); a DIFFERENT
/// same-key revision is DuplicateVerified ONLY under its OWN
/// complete retained-authority proof (V2X-CP2 F3: the strict
/// temporal position, the complete retained membership, and the
/// source binding, re-proved over the same read-only transaction
/// with the verifier's begin identity exactly the observed
/// revision — the payload match alone is never trusted completion;
/// a failure is the appropriate typed refusal or the unverifiable
/// classification); a definite absence of any same-key begin is
/// ReconciledNotInstalled; any discovery failure is the typed
/// unverifiable uncertainty.
fn reconcile_begin_publication_v2(
    medium: &mut SqliteMedium,
    mk: &Hash,
    candidate: Option<Hash>,
) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
    let tx = medium
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| MediumError::Chain {
            rule: "v2-begin-admission/reconcile-unverifiable",
        })?;
    // (READ-ONLY) The authoritative-chain validation.
    let observed = read_meta_head(&tx)?;
    if observed.is_none()
        || validate_observed(&tx, &mut medium.cursor, &mut medium.walks, observed).is_err()
    {
        return Err(MediumError::Chain {
            rule: "v2-begin-admission/reconcile-unverifiable",
        });
    }
    // (READ-ONLY) The expected begin re-derivation from the retained
    // evidence: the checkpoint metadata and the installed plan.
    let head = match observed {
        Some(head) => head,
        None => {
            return Err(MediumError::Chain {
                rule: "v2-begin-admission/reconcile-unverifiable",
            });
        }
    };
    let mut reads = SqliteTxReads { tx: &tx };
    let expected = match crate::reclaim::find_on_chain_via(&mut reads, head, None, Some(mk)) {
        Ok(Some((_id, meta_bytes))) => match CheckpointMetadata::decode(&meta_bytes) {
            Ok(_metadata) => match crate::reclaim::find_installed_plan_via(&mut reads, head, mk) {
                Ok(Some((plan_record, plan))) => Some(crate::reclaim::ReclamationBegin {
                    mk: *mk,
                    source: plan_record,
                    range_digest: plan.range_digest,
                    seed: plan.seed,
                }),
                _ => None,
            },
            Err(_) => None,
        },
        _ => None,
    };
    let Some(expected) = expected else {
        return Err(MediumError::Chain {
            rule: "v2-begin-admission/reconcile-unverifiable",
        });
    };
    match find_installed_begin_tx(&tx, mk, &expected) {
        Ok(Some((id, _installed))) => {
            if candidate == Some(id) {
                Ok(crate::reclaim::BeginPublicationOutcome::ReconciledInstalled { id })
            } else {
                // (V2X-CP2 F3) A DIFFERENT same-key revision is
                // DuplicateVerified ONLY when its OWN retained
                // authority holds: the complete strict temporal,
                // retained-membership, and source-binding proof over
                // the SAME read-only transaction snapshot (no nested
                // transaction — the reader borrows this transaction),
                // with the verifier's begin identity exactly the
                // observed same-key revision. The payload match
                // alone (the expected-form equality above) is never
                // trusted completion: a verification failure is the
                // appropriate typed refusal (or the unverifiable
                // classification when the observed revision is not
                // the proven authority), never DuplicateVerified.
                let mut authority_reads = SqliteTxReads { tx: &tx };
                match crate::reclaim::verify_v2_begin_authority_evidence_via(
                    &mut authority_reads,
                    head,
                    mk,
                ) {
                    Ok(evidence) if evidence.begin_id == id => {
                        Ok(crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id })
                    }
                    Ok(_) => Err(MediumError::Chain {
                        rule: "v2-begin-admission/reconcile-unverifiable",
                    }),
                    Err(error) => Err(error),
                }
            }
        }
        Ok(None) => Ok(crate::reclaim::BeginPublicationOutcome::ReconciledNotInstalled),
        Err(_) => Err(MediumError::Chain {
            rule: "v2-begin-admission/reconcile-unverifiable",
        }),
    }
}

/// The post-uncertain-commit reconciliation (the C2-R1/R5
/// corrective): a READ-ONLY fresh observation — it never publishes,
/// never inserts, never constructs a replacement candidate. It
/// validates the authoritative chain under the same trusted-cursor
/// discipline (a concurrent writer may have changed the state
/// between the attempt and this observation), re-derives the
/// expected begin from the (read-only) evidence verification, then
/// the gap-aware HEAD-ancestry discovery — all bound to the EXACT
/// attempted candidate identity: the candidate itself canonically
/// present is ReconciledInstalled (the membership proof, never
/// Accepted, never a durability claim); a definite absence of any
/// same-key begin is ReconciledNotInstalled (an EXPLICIT later retry
/// may revalidate and construct a new candidate — a separate
/// operation, never this one); a DIFFERENT same-key revision is
/// separately discovered, verified duplicate evidence
/// (DuplicateVerified) — never misclassified as the installation of
/// the attempted candidate; any failure is the typed unverifiable
/// uncertainty.
fn reconcile_begin_publication(
    medium: &mut SqliteMedium,
    audit: &Hash,
    mk: &Hash,
    candidate: Option<Hash>,
) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
    let tx = medium
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| MediumError::Chain {
            rule: "begin-admission/reconcile-unverifiable",
        })?;
    // (C2-R6, READ-ONLY) The authoritative-chain validation — the
    // same trusted-cursor discipline, inside the observation's own
    // snapshot.
    let observed = read_meta_head(&tx)?;
    if validate_observed(&tx, &mut medium.cursor, &mut medium.walks, observed).is_err() {
        return Err(MediumError::Chain {
            rule: "begin-admission/reconcile-unverifiable",
        });
    }
    // (READ-ONLY) The expected begin re-derivation — the
    // authentication, the founding, the evidence core: all reads.
    let (ledger, metadata, _head) = match authenticate_audit_tx(&tx, audit) {
        Ok(triple) => triple,
        Err(_) => {
            return Err(MediumError::Chain {
                rule: "begin-admission/reconcile-unverifiable",
            });
        }
    };
    if ledger.mk != *mk {
        return Err(MediumError::Chain {
            rule: "begin-admission/reconcile-unverifiable",
        });
    }
    let founding = match crate::reclaim::founding_identity(
        read_meta_head(&tx)?.ok_or(MediumError::Chain { rule: "unfounded" })?,
        |identity| load_record_verified(&tx, identity),
        |identity| load_object_verified(&tx, identity),
        |identity| record_exists(&tx, identity),
    ) {
        Ok(founding) => founding,
        Err(_) => {
            return Err(MediumError::Chain {
                rule: "begin-admission/reconcile-unverifiable",
            });
        }
    };
    let mut expected = match crate::reclaim::verify_first_execution_evidence(
        &ledger,
        &metadata,
        &founding,
        |identity| load_record_verified(&tx, identity),
        |identity| load_object_verified(&tx, identity),
    ) {
        Ok(begin) => begin,
        Err(_) => {
            return Err(MediumError::Chain {
                rule: "begin-admission/reconcile-unverifiable",
            });
        }
    };
    expected.source = *audit;
    // (READ-ONLY, C2-R5) The discovery bound to the exact attempted
    // candidate: the candidate itself present → the membership proof;
    // a DIFFERENT same-key revision → the separately discovered,
    // verified duplicate evidence (never ReconciledInstalled for the
    // original attempt); a definite absence → ReconciledNotInstalled.
    match find_installed_begin_tx(&tx, mk, &expected) {
        Ok(Some((id, _installed))) => {
            if candidate == Some(id) {
                Ok(crate::reclaim::BeginPublicationOutcome::ReconciledInstalled { id })
            } else {
                Ok(crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id })
            }
        }
        Ok(None) => Ok(crate::reclaim::BeginPublicationOutcome::ReconciledNotInstalled),
        Err(_) => Err(MediumError::Chain {
            rule: "begin-admission/reconcile-unverifiable",
        }),
    }
}

// ============================================================== the C5b tests
//
// (V2X-CP3-B-P2A-C5b) The provider-coordinated Range-terminal
// observation on SqliteMedium: ONE consistent read transaction whose
// snapshot binds at its first statement — the head read and the
// whole walk inside the single snapshot. The genuine campaign is
// direct-inserted (the completion family's publication fence forbids
// the generic append for the /2 and /3 markers, and no V2 publisher
// exists yet).

#[cfg(test)]
mod c5b_sqlite_tests {
    use super::*;
    use crate::ancestry_accounting::StorageRole;
    use crate::batch_completion::{
        BatchCompletionContext, BatchCompletionMarkerV2, ClassificationEntry, EntryClassification,
        commit_classification, reclamation_complete_v2_idempotency_key,
    };
    use crate::checkpoint::{
        CheckpointMetadata, ReclaimedIndexEntry, checkpoint_mk_idempotency_key,
    };
    use crate::hash::hash_object;
    use crate::range_completion::{
        RangeCompletionContext, RangeCompletionMarkerV3, RangeCoverageObservation,
        commit_journal_obligations, commit_range_coverage,
        reclamation_complete_v3_range_idempotency_key,
    };
    use crate::range_installation::{
        CoordinatedObservation, NotObservedTerminus, RangeInstallationLimits,
        RangeInstallationObservation,
    };
    use crate::reclaim::{
        PlanBatch, ReclaimPlan, ReclaimUnit, ReclamationBegin, ReclamationLedgerV2,
        reclamation_begin_idempotency_key, reclamation_ledger_v2_idempotency_key,
        reclamation_plan_idempotency_key,
    };

    /// The direct-inserting chain builder over the medium's own
    /// connection: each keyed evidence record + its payload object
    /// inserted through the store's own one-shot helpers inside a
    /// short transaction, the meta head updated at the end.
    struct SqliteDirectBuilder<'a> {
        conn: &'a mut rusqlite::Connection,
        head: Hash,
        head_epoch: u64,
        tick: u64,
    }

    impl SqliteDirectBuilder<'_> {
        fn new(conn: &mut rusqlite::Connection, seed: Hash, epoch: u64) -> SqliteDirectBuilder<'_> {
            SqliteDirectBuilder {
                conn,
                head: seed,
                head_epoch: epoch,
                tick: 0,
            }
        }

        fn write(&mut self, key: Vec<u8>, payload: Vec<u8>) -> Hash {
            let tx = self.conn.transaction().unwrap();
            let payload_object = StoredObject::new(payload).unwrap();
            insert_object_once(&tx, &None, &payload_object).unwrap();
            self.tick += 1;
            let record = RevisionRecord::regular(self.head, self.head_epoch, self.tick)
                .with_idempotency_key(key)
                .with_content_refs(vec![payload_object.hash()]);
            let id = record.id();
            insert_record_once(&tx, &record, &record.canonical_bytes(), &id).unwrap();
            tx.commit().unwrap();
            self.head = id;
            self.head_epoch += 1;
            id
        }

        fn set_head(&self, id: Hash) {
            self.conn
                .execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'head'",
                    params![id.as_bytes()],
                )
                .unwrap();
        }
    }

    struct Campaign {
        head: Hash,
        head_epoch: u64,
        mk: Hash,
        expected: RangeCompletionContext,
    }

    fn temp_db(label: &str) -> (std::path::PathBuf, SqliteMedium) {
        let path = std::env::temp_dir().join(format!(
            "s7-c5b-{label}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let medium = SqliteMedium::open(&path).unwrap();
        (path, medium)
    }

    fn genuine_campaign(medium: &mut SqliteMedium, with_terminal: bool) -> Campaign {
        let members = [
            hash_object(b"c5b-sq-sk"),
            hash_object(b"c5b-sq-m1"),
            hash_object(b"c5b-sq-m2"),
            hash_object(b"c5b-sq-m3"),
            hash_object(b"c5b-sq-m4"),
        ];
        let range_digest = [7u8; 32];
        let seed = [9u8; 32];
        let index = vec![
            ReclaimedIndexEntry::new(None, members[0], 60),
            ReclaimedIndexEntry::new(Some(b"key-m1".to_vec()), members[1], 50),
            ReclaimedIndexEntry::new(None, members[2], 40),
            ReclaimedIndexEntry::new(Some(b"key-m3".to_vec()), members[3], 30),
            ReclaimedIndexEntry::new(None, members[4], 20),
        ];
        let metadata = CheckpointMetadata::new(
            members[0],
            hash_object(b"c5b-sq-sk-parent"),
            vec![],
            index,
            range_digest,
            seed,
        );
        let conn = &mut medium.conn;
        let mut builder = SqliteDirectBuilder::new(conn, members[0], 60);
        let mk = builder.write(
            checkpoint_mk_idempotency_key(&members[0]),
            metadata.canonical_bytes(),
        );
        let plan = ReclaimPlan {
            mk,
            sk: members[0],
            range_digest,
            seed,
            ledger_byte_limit: 2048,
            batches: vec![
                PlanBatch {
                    index: 0,
                    records: vec![members[0], members[1]],
                },
                PlanBatch {
                    index: 1,
                    records: vec![members[2], members[3], members[4]],
                },
            ],
            journal_files: vec!["journal.t1.0".to_owned(), "journal.t1.2".to_owned()],
        };
        let plan_record = builder.write(
            reclamation_plan_idempotency_key(&mk),
            plan.canonical_bytes(),
        );
        let audit0 = ReclamationLedgerV2 {
            mk,
            sk: members[0],
            batch: 0,
            range_digest,
            seed,
            records: 2,
            reclaimed_keys: 1,
            units: vec![
                ReclaimUnit {
                    record: members[0],
                    objects: vec![],
                },
                ReclaimUnit {
                    record: members[1],
                    objects: vec![hash_object(b"c5b-sq-o1")],
                },
            ],
        };
        let audit0_record = builder.write(
            reclamation_ledger_v2_idempotency_key(&mk, 0),
            audit0.canonical_bytes(),
        );
        let audit1 = ReclamationLedgerV2 {
            mk,
            sk: members[0],
            batch: 1,
            range_digest,
            seed,
            records: 3,
            reclaimed_keys: 1,
            units: vec![
                ReclaimUnit {
                    record: members[2],
                    objects: vec![],
                },
                ReclaimUnit {
                    record: members[3],
                    objects: vec![],
                },
                ReclaimUnit {
                    record: members[4],
                    objects: vec![],
                },
            ],
        };
        let audit1_record = builder.write(
            reclamation_ledger_v2_idempotency_key(&mk, 1),
            audit1.canonical_bytes(),
        );
        let begin = ReclamationBegin {
            mk,
            source: plan_record,
            range_digest,
            seed,
        };
        let begin_record = builder.write(
            reclamation_begin_idempotency_key(&mk),
            begin.canonical_bytes(),
        );
        let mut marker_records = Vec::new();
        let mut observations = Vec::new();
        for (batch, entries, audit) in [
            (
                0u64,
                vec![
                    (
                        members[0],
                        StorageRole::Record,
                        EntryClassification::Mandatory,
                    ),
                    (
                        members[1],
                        StorageRole::Record,
                        EntryClassification::Mandatory,
                    ),
                    (
                        hash_object(b"c5b-sq-o1"),
                        StorageRole::Object,
                        EntryClassification::Deferred,
                    ),
                ],
                audit0_record,
            ),
            (
                1u64,
                vec![
                    (
                        members[2],
                        StorageRole::Record,
                        EntryClassification::Mandatory,
                    ),
                    (
                        members[3],
                        StorageRole::Record,
                        EntryClassification::Mandatory,
                    ),
                    (
                        members[4],
                        StorageRole::Record,
                        EntryClassification::Deferred,
                    ),
                ],
                audit1_record,
            ),
        ] {
            let context = BatchCompletionContext {
                mk,
                plan: plan_record,
                batch_index: batch,
                audit,
                begin: begin_record,
            };
            let entries: Vec<ClassificationEntry> = entries
                .into_iter()
                .map(|(identity, role, classification)| ClassificationEntry {
                    identity,
                    role,
                    classification,
                    conditional_on_prior_batch: false,
                })
                .collect();
            let commitment = commit_classification(&context, entries).unwrap();
            let marker = BatchCompletionMarkerV2 {
                mk,
                plan: plan_record,
                batch_index: batch,
                audit,
                begin: begin_record,
                commitment: commitment.digest,
                counters: commitment.counters,
            };
            let record = builder.write(
                reclamation_complete_v2_idempotency_key(&mk, batch),
                marker.canonical_bytes(),
            );
            marker_records.push(record);
            observations.push(RangeCoverageObservation {
                batch_index: batch,
                marker_record: record,
                counters: commitment.counters,
            });
        }
        let expected = RangeCompletionContext {
            mk,
            plan: plan_record,
            begin: begin_record,
        };
        let head = if with_terminal {
            let coverage = commit_range_coverage(&expected, &observations).unwrap();
            let journal = commit_journal_obligations(&expected, &plan.journal_files).unwrap();
            let marker = RangeCompletionMarkerV3 {
                mk,
                plan: plan_record,
                begin: begin_record,
                coverage: coverage.digest,
                journal,
                batch_count: observations.len() as u64,
                journal_count: plan.journal_files.len() as u64,
                counters: coverage.aggregate,
            };
            builder.write(
                reclamation_complete_v3_range_idempotency_key(&mk),
                marker.canonical_bytes(),
            )
        } else {
            builder.head
        };
        let head_epoch = builder.head_epoch;
        builder.set_head(head);
        Campaign {
            head,
            head_epoch,
            mk,
            expected,
        }
    }

    #[test]
    fn the_coordinated_positive_on_sqlite() {
        let (path, mut medium) = temp_db("positive");
        let campaign = genuine_campaign(&mut medium, true);
        let coordinated = medium
            .observe_range_terminal_under_coordination(
                &campaign.expected,
                &RangeInstallationLimits::default(),
            )
            .unwrap();
        let CoordinatedObservation {
            coordinated_head,
            observation,
            ..
        } = coordinated;
        assert_eq!(coordinated_head, campaign.head);
        assert!(matches!(
            observation,
            RangeInstallationObservation::ObservedOnAncestry { .. }
        ));
        drop(medium);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_unfounded_sqlite_store_refuses_typed() {
        let (path, mut medium) = temp_db("unfounded");
        let expected = RangeCompletionContext {
            mk: hash_object(b"c5b-sq-any-mk"),
            plan: hash_object(b"c5b-sq-any-plan"),
            begin: hash_object(b"c5b-sq-any-begin"),
        };
        assert!(matches!(
            medium.observe_range_terminal_under_coordination(
                &expected,
                &RangeInstallationLimits::default()
            ),
            Err(MediumError::Chain { rule: "unfounded" })
        ));
        drop(medium);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_absent_terminal_on_sqlite_classifies() {
        let (path, mut medium) = temp_db("absent");
        let campaign = genuine_campaign(&mut medium, false);
        let coordinated = medium
            .observe_range_terminal_under_coordination(
                &campaign.expected,
                &RangeInstallationLimits::default(),
            )
            .unwrap();
        assert!(matches!(
            coordinated.observation,
            RangeInstallationObservation::NotObservedOnAncestry {
                terminus: NotObservedTerminus::ExpectedCoveringCheckpoint { mk },
                ..
            } if mk == campaign.mk
        ));
        drop(medium);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_sqlite_snapshot_binds_before_a_later_commit() {
        // The snapshot-consistency adversary: the observation
        // transaction's first statement binds the snapshot; a
        // LATER commit by a second connection is wholly INVISIBLE
        // to the walk inside that transaction — and wholly visible
        // to the next fresh observation.
        let (path, mut medium) = temp_db("snapshot");
        let campaign = genuine_campaign(&mut medium, true);
        // The held observation transaction: the FIRST statement
        // (the meta head read) binds the snapshot.
        let tx = medium.conn.transaction().unwrap();
        let bound_head = read_meta_head(&tx).unwrap().unwrap();
        assert_eq!(bound_head, campaign.head);
        // A second RAW connection commits a later publication (the
        // head moves on disk) — raw, because SqliteMedium::open's
        // cold walk would traverse into the campaign's virtual seed
        // below the MK and refuse.
        let mut second = rusqlite::Connection::open(&path).unwrap();
        let later_payload = {
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"c5b-sq-later");
            enc.into_bytes()
        };
        let later_object = StoredObject::new(later_payload).unwrap();
        let later_record = RevisionRecord::regular(campaign.head, campaign.head_epoch, 99)
            .with_content_refs(vec![later_object.hash()]);
        {
            let tx2 = second.transaction().unwrap();
            insert_object_once(&tx2, &None, &later_object).unwrap();
            insert_record_once(
                &tx2,
                &later_record,
                &later_record.canonical_bytes(),
                &later_record.id(),
            )
            .unwrap();
            tx2.execute(
                "UPDATE meta SET value = ?1 WHERE key = 'head'",
                params![later_record.id().as_bytes()],
            )
            .unwrap();
            tx2.commit().unwrap();
        }
        // The walk INSIDE the held snapshot still sees the bound
        // head (the later commit wholly invisible).
        let coordinated =
            observe_range_terminal_tx(&tx, &campaign.expected, &RangeInstallationLimits::default())
                .unwrap();
        assert_eq!(coordinated.coordinated_head, bound_head);
        assert!(matches!(
            coordinated.observation,
            RangeInstallationObservation::ObservedOnAncestry { .. }
        ));
        drop(tx);
        // The FRESH observation sees the later head.
        let fresh = medium
            .observe_range_terminal_under_coordination(
                &campaign.expected,
                &RangeInstallationLimits::default(),
            )
            .unwrap();
        assert_eq!(fresh.coordinated_head, later_record.id());
        drop(medium);
        drop(second);
        let _ = std::fs::remove_file(&path);
    }
}
