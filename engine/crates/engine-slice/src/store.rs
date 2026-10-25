//! The PP-B1 provider-selection seam: the explicit store location and
//! the medium union that lets the REAL Stage 5 persistence and
//! recovery paths (the B3 pipeline, the B4/B5 crossing lifecycle) run
//! against either provider behind the unchanged `CanonicalMedium`
//! contract. **No silent selection ever occurs**: the existing
//! directory-based constructors keep the `LocalFileMedium` behavior
//! byte-identical; the SQLite provider is engaged only through the
//! explicit `StoreLocation::Database` form (or a caller that has
//! already migrated) — a fresh database is never silently opened when
//! an existing campaign should have been recovered or migrated (the
//! crossing's open/recover always read the location's actual head
//! first, and a SQLite database at a fresh path IS unfounded until the
//! crossing's founding append runs — the same first-open semantics as
//! the file medium).

use std::path::PathBuf;

use engine_persist::{
    AppendOutcome, CanonicalMedium, ChainSummary, LocalFileMedium, MediumError, Reconciliation,
    RevisionRecord, SqliteMedium, StoredObject, SyncTier,
};

/// Where a campaign's canonical store lives — the explicit provider
/// selection (PP-B1's integration seam; the design's "an explicit
/// provider option").
#[derive(Debug, Clone)]
pub enum StoreLocation {
    /// The legacy file-backed store root (the `LocalFileMedium`
    /// layout: `HEAD`, `HEAD.lock`, `objects/`, `quarantine/`).
    Directory(PathBuf),
    /// The SQLite campaign database (one database per campaign, WAL +
    /// `synchronous=FULL` — the ratified D-PP-1 production provider).
    Database(PathBuf),
}

impl StoreLocation {
    /// The path's display form (diagnostics).
    pub fn display_path(&self) -> String {
        match self {
            StoreLocation::Directory(path) => path.display().to_string(),
            StoreLocation::Database(path) => path.display().to_string(),
        }
    }
}

/// The medium union: the `CanonicalMedium` contract over either
/// provider, by direct delegation. The pipeline and the crossing
/// operate on this type unchanged — the provider choice is made once,
/// at the location.
pub enum SessionStore {
    /// The reference implementation (the file medium).
    Local(LocalFileMedium),
    /// The production provider (PP-B1, the owner-ratified D-PP-1).
    Sqlite(SqliteMedium),
}

impl SessionStore {
    /// Opens the store at the location (the provider selection is the
    /// location's shape — never inferred from content, never silent).
    pub fn open(location: &StoreLocation) -> Result<Self, MediumError> {
        match location {
            StoreLocation::Directory(root) => {
                Ok(SessionStore::Local(LocalFileMedium::open(root.clone())?))
            }
            StoreLocation::Database(path) => {
                Ok(SessionStore::Sqlite(SqliteMedium::open(path.clone())?))
            }
        }
    }

    /// The location this store was opened from (the fresh-handle
    /// publications and the recovery re-open reuse it).
    pub fn location(&self) -> StoreLocation {
        match self {
            SessionStore::Local(medium) => StoreLocation::Directory(PathBuf::from(medium.root())),
            SessionStore::Sqlite(medium) => StoreLocation::Database(medium.db_path().to_path_buf()),
        }
    }

    /// The graceful lifecycle boundary: the ratified `TRUNCATE`
    /// checkpoint for the SQLite provider; a no-op for the file
    /// medium (its every publication already fsyncs by the nine-step
    /// contract). Failures are typed, never swallowed.
    pub fn checkpoint(&mut self) -> Result<(), MediumError> {
        match self {
            SessionStore::Local(_) => Ok(()),
            SessionStore::Sqlite(medium) => medium.checkpoint_truncate(),
        }
    }
}

impl CanonicalMedium for SessionStore {
    fn head(&mut self) -> Result<Option<engine_persist::Hash>, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.head(),
            SessionStore::Sqlite(medium) => medium.head(),
        }
    }

    fn append(
        &mut self,
        record: &RevisionRecord,
        objects: &[StoredObject],
        expected_parent: Option<engine_persist::Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.append(record, objects, expected_parent),
            SessionStore::Sqlite(medium) => medium.append(record, objects, expected_parent),
        }
    }

    fn read_record(&mut self, id: &engine_persist::Hash) -> Result<RevisionRecord, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.read_record(id),
            SessionStore::Sqlite(medium) => medium.read_record(id),
        }
    }

    fn read_object(&mut self, hash: &engine_persist::Hash) -> Result<Vec<u8>, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.read_object(hash),
            SessionStore::Sqlite(medium) => medium.read_object(hash),
        }
    }

    fn validate(&mut self) -> Result<ChainSummary, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.validate(),
            SessionStore::Sqlite(medium) => medium.validate(),
        }
    }

    fn reconcile_unknown(
        &mut self,
        candidate: &engine_persist::Hash,
    ) -> Result<Reconciliation, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.reconcile_unknown(candidate),
            SessionStore::Sqlite(medium) => medium.reconcile_unknown(candidate),
        }
    }

    fn durability_tier(&self) -> SyncTier {
        match self {
            SessionStore::Local(medium) => medium.durability_tier(),
            SessionStore::Sqlite(medium) => medium.durability_tier(),
        }
    }

    fn reclaim(&mut self, units: &[engine_persist::ReclaimUnit]) -> Result<u64, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.reclaim(units),
            SessionStore::Sqlite(medium) => medium.reclaim(units),
        }
    }
}
