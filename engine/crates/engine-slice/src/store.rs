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
    RevisionRecord, SqliteMedium, SyncTier,
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

/// The canonical-medium SOURCE — PP-B4 checkpoint 1's ownership seam:
/// what the crossing opens its per-operation medium handles FROM. The
/// legacy instantiation is the [`StoreLocation`] itself (fresh direct
/// provider handles — exactly the accepted behavior); a future
/// orchestrated instantiation supplies the single I/O worker's proxy
/// source, so every in-session canonical access — the snapshot
/// publications, the checkpoint SK/MK walks, the reclamation, the
/// rotation rebuild, and the lifecycle checkpoint — routes through
/// the one injected medium instead of reopening independent provider
/// handles. `Clone` because the crossing owns its source from a
/// borrowed constructor argument, exactly as it cloned the store
/// location before.
pub trait MediumSource: Clone {
    /// The medium type this source opens.
    type Medium: CanonicalMedium;

    /// Opens one per-operation medium handle. Each provider's own
    /// coordination is per-operation, so additional handles serialize
    /// safely alongside one another (the accepted discipline); the
    /// future worker instantiation's "handles" are queue proxies to
    /// the one serialization point.
    fn open_medium(&self) -> Result<Self::Medium, MediumError>;

    /// Whether the source's medium path is usable for AUTHORITATIVE
    /// progression (PP-B4 checkpoint 2's poison surface). The default
    /// is always true — a store location cannot die; the worker-backed
    /// source reports its shared health cell, so a dead or poisoned
    /// worker fails every authoritative crossing operation closed
    /// (journal-only deactivations included) until fresh recovery.
    fn alive(&self) -> bool {
        true
    }
}

impl MediumSource for StoreLocation {
    type Medium = SessionStore;

    fn open_medium(&self) -> Result<SessionStore, MediumError> {
        SessionStore::open(self)
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

    fn append_admitted(
        &mut self,
        admitted: engine_persist::medium::AdmittedAppend,
        expected_parent: Option<engine_persist::Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        // The receipt's dispatch through the store: the value is
        // UNFORGEABLE (constructed only inside engine-persist after
        // its verification) — the forwarding is pure delegation.
        match self {
            SessionStore::Local(medium) => medium.append_admitted(admitted, expected_parent),
            SessionStore::Sqlite(medium) => medium.append_admitted(admitted, expected_parent),
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

    /// (V2-W, W3A) The union medium delegates to the selected
    /// provider — the SQLite-backed store reports the runtime
    /// `SQLITE_MAX_LENGTH`, the local-backed store the compile-time
    /// object bound.
    fn effective_object_byte_limit(&mut self) -> Result<u64, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.effective_object_byte_limit(),
            SessionStore::Sqlite(medium) => medium.effective_object_byte_limit(),
        }
    }

    fn reclaim(
        &mut self,
        directive: &engine_persist::ReclaimDirective,
    ) -> Result<engine_persist::ReclaimOutcome, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.reclaim(directive),
            SessionStore::Sqlite(medium) => medium.reclaim(directive),
        }
    }

    fn reclaim_pending(
        &mut self,
        directive: &engine_persist::ReclaimDirective,
    ) -> Result<bool, MediumError> {
        match self {
            SessionStore::Local(medium) => medium.reclaim_pending(directive),
            SessionStore::Sqlite(medium) => medium.reclaim_pending(directive),
        }
    }

    fn checkpoint(&mut self) -> Result<(), MediumError> {
        SessionStore::checkpoint(self)
    }

    fn begin_reclamation(
        &mut self,
        audit: &engine_persist::Hash,
        mk: &engine_persist::Hash,
        tick: u64,
    ) -> Result<engine_persist::reclaim::BeginPublicationOutcome, MediumError> {
        // (OC-C3 CP-1) The direct-source routing of the trusted
        // begin admission — the provider's own accepted
        // implementation, unchanged.
        match self {
            SessionStore::Local(medium) => medium.begin_reclamation(audit, mk, tick),
            SessionStore::Sqlite(medium) => medium.begin_reclamation(audit, mk, tick),
        }
    }

    fn begin_reclamation_v2(
        &mut self,
        mk: &engine_persist::Hash,
        tick: u64,
    ) -> Result<engine_persist::reclaim::BeginPublicationOutcome, MediumError> {
        // (V2X-CP2) The direct-source routing of the trusted V2
        // plan-bound begin admission — the provider's own
        // implementation, unchanged.
        match self {
            SessionStore::Local(medium) => medium.begin_reclamation_v2(mk, tick),
            SessionStore::Sqlite(medium) => medium.begin_reclamation_v2(mk, tick),
        }
    }

    fn verify_begin_authority_v2(
        &mut self,
        mk: &engine_persist::Hash,
    ) -> Result<engine_persist::reclaim::BeginAuthority, MediumError> {
        // (V2X-CP2) The direct-source routing of the retained-evidence
        // V2 begin authority.
        match self {
            SessionStore::Local(medium) => medium.verify_begin_authority_v2(mk),
            SessionStore::Sqlite(medium) => medium.verify_begin_authority_v2(mk),
        }
    }

    fn observe_range_terminal_under_coordination(
        &mut self,
        expected: &engine_persist::range_completion::RangeCompletionContext,
        limits: &engine_persist::range_installation::RangeInstallationLimits,
    ) -> Result<engine_persist::range_installation::CoordinatedObservation, MediumError> {
        // (V2X-CP3-B-P2A-C5b) The direct-source routing of the
        // provider-coordinated Range-terminal observation.
        match self {
            SessionStore::Local(medium) => {
                medium.observe_range_terminal_under_coordination(expected, limits)
            }
            SessionStore::Sqlite(medium) => {
                medium.observe_range_terminal_under_coordination(expected, limits)
            }
        }
    }

    fn verify_begin_authority(
        &mut self,
        audit: &engine_persist::Hash,
        mk: &engine_persist::Hash,
    ) -> Result<engine_persist::reclaim::BeginAuthority, MediumError> {
        // (OC-C3 CP-1) The direct-source routing of the
        // installed-begin durability authority.
        match self {
            SessionStore::Local(medium) => medium.verify_begin_authority(audit, mk),
            SessionStore::Sqlite(medium) => medium.verify_begin_authority(audit, mk),
        }
    }
}
