//! `LocalFileMedium` — the §5.3 nine-step conditional-append protocol
//! on a local directory, with the §5.3.2 trusted-cursor validation
//! model and the §5.3.3 v1.4 orphan protocol.
//!
//! **Layout** (the documented campaign directory):
//!
//! ```text
//! <root>/HEAD        the authoritative head pointer (head/1)
//! <root>/HEAD.lock   the persistent OS-managed coordination file —
//!                    created once by the medium, locked per operation,
//!                    NEVER unlinked or recreated during ordinary
//!                    release; process death releases ownership
//! <root>/HEAD.tmp    transient publication scratch (only meaningful
//!                    under the lock; overwritten by each publish)
//! <root>/objects/    immutable content-addressed objects (<hex>)
//! <root>/quarantine/ evidence preserved, never deleted
//! ```
//!
//! **The nine steps** (append): acquire `HEAD.lock` → validate the
//! observed HEAD (trusted cursor: O(1) on the verified head, suffix
//! walk on divergence, cold walk otherwise) → compare with the
//! caller's expected parent → reject stale writers without altering
//! HEAD → write every object the candidate needs (insert-once,
//! verify-on-retry, quarantine-then-rewrite on an unreferenced
//! mismatch, fail-closed on a referenced mismatch) with file fsyncs
//! and the Unix objects-directory fsync → publish HEAD via
//! `HEAD.tmp` + fsync + rename (the protocol's **linearization
//! point**) → post-rename root-directory fsync (Unix; the documented
//! Windows sequence offers no directory flush — the tier carries the
//! limitation) → release the lock after a defined outcome → reconcile
//! uncertainty on demand. **Every I/O failure from the first object
//! write onward is the `Unknown` outcome** — never `Accepted`, never
//! a clean retryable error hiding an uncertain commit.
//!
//! **Validation (§5.3.2):** session open performs the full founding →
//! HEAD walk (every record and every referenced object hash-verified)
//! and an unreferenced-object sweep that quarantines under the lock.
//! Within the session, an append to the already-verified head does
//! O(1) validation work (no records walked — the deterministic
//! counter in [`WalkStats`] is asserted by the tests); external
//! divergence suffix-walks only the new records; integrity failure or
//! a non-descendant observed head fails closed (divergent histories
//! are detected, never merged). The cursor is session-scoped in-memory
//! derived state — **no persistent trusted-cache file**. Its
//! `seen_keys`/`reachable` indexes are O(chain) memory, documented
//! honestly; the O(1) claim is about per-append walk work.
//!
//! **Orphans (§5.3.3 v1.4):** a referenced object failing its hash is
//! fail-closed evidence (never replaced, never deleted); an
//! unreferenced questionable object is quarantined with its bytes and
//! a reason file — never auto-deleted; permanent reclamation is not
//! authorized in this batch. `HEAD.tmp` is protocol scratch, not
//! evidence: the rename's atomicity means HEAD already reflects the
//! outcome, so a stale `HEAD.tmp` is simply overwritten by the next
//! publish.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use engine_core::wire::MAX_BYTE_LEN;
use engine_core::{Decoder, Encoder};

use crate::fault::{FaultControl, FaultPoint};
use crate::hash::{Hash, hash_object};
use crate::medium::{
    AppendOutcome, CanonicalMedium, ChainSummary, MediumError, Reconciliation, StoredObject,
    SyncTier, UnknownReason,
};
use crate::record::{AuthorityMode, RecordKind, RevisionRecord};

/// The HEAD pointer's domain (a persisted wire format; visible to
/// in-crate tests that hand-build stores).
pub(crate) const HEAD_DOMAIN: &str = "sektor7/engine-persist/head/1";
pub(crate) const HEAD_VERSION: u32 = 1;

/// The on-disk layout (documented in the module docs; visible to
/// in-crate tests).
pub(crate) const OBJECTS_DIR: &str = "objects";
pub(crate) const QUARANTINE_DIR: &str = "quarantine";
pub(crate) const HEAD_FILE: &str = "HEAD";
pub(crate) const HEAD_LOCK: &str = "HEAD.lock";
pub(crate) const HEAD_TMP: &str = "HEAD.tmp";

/// The §6.1 offline migration's seal marker (PP-B1): its presence
/// means the directory was migrated to a SQLite campaign database
/// and is archived evidence — new binaries refuse to open it as a
/// source (binaries predating the tombstone cannot be made to honor
/// it; the ratified documented operational limitation).
pub(crate) const MIGRATED_MARKER: &str = "MIGRATED";

/// Documented maximum for a stored object's size (matches the Batch 1
/// byte-length maximum; enforced by stat before any read allocates).
const MAX_OBJECT_BYTES: u64 = MAX_BYTE_LEN;

/// The ephemeral trusted cursor (§5.3.2) — session-scoped derived
/// state rebuilt by the cold walk; never persisted to a file.
struct Cursor {
    head: Hash,
    epoch: u64,
    /// Idempotency keys seen across the chain (session index for O(1)
    /// duplicate checks at append; O(chain) memory, documented).
    seen_keys: BTreeSet<Vec<u8>>,
    /// Every record id and content hash reachable from HEAD.
    reachable: BTreeSet<Hash>,
    /// The chain's revision ids alone (dependency resolution requires
    /// a *prior revision*, never a content hash that happens to be
    /// reachable).
    record_ids: BTreeSet<Hash>,
    founding: Hash,
    authority: AuthorityMode,
    campaign_id: String,
    content_objects: u64,
}

/// How an in-window object-write failure classifies.
enum ObjectWriteFailure {
    Unknown(UnknownReason),
    FailClosed(MediumError),
}

/// Phase-separated development timings (observational, nanoseconds;
/// not performance claims — B5 owns the hardware analysis).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppendTimings {
    /// Record canonicalization + structural validation + hashing.
    pub encode_nanos: u64,
    /// Object writes, file syncs, and the objects-directory sync.
    pub object_write_sync_nanos: u64,
    /// HEAD.tmp write + fsync + rename + root-directory sync.
    pub head_publish_nanos: u64,
    /// The observed-HEAD validation phase (cursor work).
    pub validation_nanos: u64,
    /// The last lock-held reconciliation.
    pub reconcile_nanos: u64,
}

/// Deterministic validation-work counters (asserted by the O(1)-cursor
/// tests; timing assertions are never used for this property).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkStats {
    /// Records verified in the last cold walk.
    pub cold_walk_records: u64,
    /// Records walked during the last append's validation phase
    /// (0 for an append to the already-verified head).
    pub last_append_walked: u64,
    /// Records walked during the last reconciliation.
    pub last_reconcile_walked: u64,
}

/// The local file-backed canonical medium.
pub struct LocalFileMedium {
    root: PathBuf,
    lock_file: File,
    cursor: Option<Cursor>,
    fault: Option<FaultControl>,
    timings: AppendTimings,
    walks: WalkStats,
    platform_tier: SyncTier,
}

impl LocalFileMedium {
    /// Opens (creating the layout) and performs the session open: the
    /// cold founding → HEAD validation and the unreferenced-object
    /// sweep, both under the lock.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, MediumError> {
        Self::open_with(root, None)
    }

    /// Test-infrastructure constructor with the crash-window fault
    /// seam armed (production uses [`Self::open`]).
    pub fn open_with(
        root: impl Into<PathBuf>,
        fault: Option<FaultControl>,
    ) -> Result<Self, MediumError> {
        let root = root.into();
        // The §6.1 seal admission (PP-B1): a MIGRATED tombstone means
        // this directory is archived evidence, never a live source —
        // refuse before touching anything.
        if root.join(MIGRATED_MARKER).is_file() {
            return Err(MediumError::Sealed {
                detail: format!(
                    "the store at {} was migrated offline — the directory is \
                     archived evidence, never a live campaign source",
                    root.display()
                ),
            });
        }
        let objects = root.join(OBJECTS_DIR);
        let quarantine = root.join(QUARANTINE_DIR);
        fs::create_dir_all(&objects).map_err(|e| io_err("create objects dir", &objects, e))?;
        fs::create_dir_all(&quarantine)
            .map_err(|e| io_err("create quarantine dir", &quarantine, e))?;
        let lock_path = root.join(HEAD_LOCK);
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| io_err("open HEAD.lock", &lock_path, e))?;
        let mut medium = LocalFileMedium {
            root,
            lock_file,
            cursor: None,
            fault,
            timings: AppendTimings::default(),
            walks: WalkStats::default(),
            platform_tier: platform_tier(),
        };
        // Session open — no fault points fire here: the crash matrix
        // targets the append/reconcile paths.
        medium.lock()?;
        let opened = match medium.read_head_file() {
            Ok(Some(id)) => {
                medium.cold_walk_from(id)?;
                medium.sweep_unreferenced()?;
                Ok(())
            }
            Ok(None) => {
                // No authoritative HEAD (never founded, or an
                // interrupted founding attempt): nothing is
                // referenced — sweep every object into quarantine,
                // preserving the attempt's evidence without
                // manufacturing a founding revision.
                medium.sweep_unreferenced().map(|_| ())
            }
            Err(error) => Err(error),
        };
        let _ = medium.unlock();
        opened.map(|()| medium)
    }

    /// The last append/reconcile's phase timings (observational).
    pub fn timings(&self) -> AppendTimings {
        self.timings
    }

    /// The deterministic validation-work counters.
    pub fn walk_stats(&self) -> WalkStats {
        self.walks
    }

    /// The store root path (the PP-B1 provider seam's location
    /// reporting — diagnostics and the fresh-handle reuse).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The campaign identity established by the session open (or a
    /// later validated append) — founding id, authority mode, and
    /// campaign id, available without re-walking. `None` before the
    /// store is founded.
    pub fn campaign(&self) -> Option<(Hash, AuthorityMode, &str)> {
        self.cursor.as_ref().map(|cursor| {
            (
                cursor.founding,
                cursor.authority,
                cursor.campaign_id.as_str(),
            )
        })
    }

    // ------------------------------------------------------------ locking

    fn lock(&self) -> Result<(), MediumError> {
        self.lock_file
            .lock()
            .map_err(|e| io_err("lock HEAD.lock", &self.root.join(HEAD_LOCK), e))
    }

    fn unlock(&self) -> Result<(), MediumError> {
        self.lock_file
            .unlock()
            .map_err(|e| io_err("unlock HEAD.lock", &self.root.join(HEAD_LOCK), e))
    }

    // ----------------------------------------------------------- internals

    fn object_path(&self, hash: &Hash) -> PathBuf {
        self.root.join(OBJECTS_DIR).join(hash.hex())
    }

    fn load_object(&self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
        let path = self.object_path(hash);
        let metadata = fs::metadata(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => MediumError::MissingObject { hash: *hash },
            _ => io_err("stat object", &path, e),
        })?;
        if metadata.len() > MAX_OBJECT_BYTES {
            return Err(MediumError::Format(engine_core::SaveError::Limit {
                section: "object-store",
                kind: "byte-length",
                value: metadata.len(),
                max: MAX_OBJECT_BYTES,
            }));
        }
        let bytes = fs::read(&path).map_err(|e| io_err("read object", &path, e))?;
        let observed = hash_object(&bytes);
        if observed != *hash {
            return Err(MediumError::HashMismatch {
                expected: *hash,
                observed,
                context: "object-store",
            });
        }
        Ok(bytes)
    }

    /// Loads an object a validation walk reached by reference: a hash
    /// mismatch here is referenced corruption — fail closed with the
    /// specific condition (the file is never replaced or deleted).
    fn load_referenced(&self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
        self.load_object(hash).map_err(|error| match error {
            MediumError::HashMismatch { .. } => {
                MediumError::ReferencedObjectCorrupt { hash: *hash }
            }
            other => other,
        })
    }

    fn read_head_file(&self) -> Result<Option<Hash>, MediumError> {
        read_head_at(&self.root)
    }

    /// A pre-window fault check — an injected failure is a clean,
    /// retryable error (nothing has been written).
    fn fault_pre(&self, point: FaultPoint) -> Result<(), MediumError> {
        match &self.fault {
            Some(control) => control.check(point),
            None => Ok(()),
        }
    }

    /// An in-window fault check — an injected failure becomes the
    /// `Unknown` outcome for the phase's reason.
    fn fault_window(&self, point: FaultPoint, reason: UnknownReason) -> Result<(), UnknownReason> {
        match &self.fault {
            Some(control) => control.check(point).map_err(|_| reason),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------- append steps

    /// Steps 2–4 plus the record-level coherence checks, then steps
    /// 5–7 with every in-window failure classified as `Unknown`.
    fn append_locked(
        &mut self,
        record: &RevisionRecord,
        objects: &[StoredObject],
        expected_parent: Option<Hash>,
        record_bytes: &[u8],
        record_id: &Hash,
    ) -> Result<AppendOutcome, MediumError> {
        self.fault_pre(FaultPoint::AfterLock)?;

        // Step 2: validate the observed HEAD.
        let validation_started = Instant::now();
        let observed = self.read_head_file()?;
        self.validate_observed(observed)?;
        self.timings.validation_nanos = elapsed_nanos(validation_started);
        self.fault_pre(FaultPoint::AfterValidation)?;

        // Steps 3–4: compare and reject stale writers.
        if observed != expected_parent {
            return Ok(AppendOutcome::RejectedStaleParent { observed });
        }
        match (observed, &record.kind) {
            (None, RecordKind::Founding { .. }) => {
                if record.epoch != 0 || record.parent.is_some() {
                    return Err(MediumError::Chain {
                        rule: "kind-incoherent",
                    });
                }
                if !record.depends_on.is_empty() {
                    return Err(MediumError::Chain {
                        rule: "depends-on-unresolvable",
                    });
                }
            }
            (Some(head), RecordKind::Regular) => {
                let cursor = self
                    .cursor
                    .as_ref()
                    .expect("validation established the cursor");
                // Checked arithmetic: an epoch at u64::MAX has no
                // valid successor and must fail closed, never wrap.
                if cursor.epoch.checked_add(1) != Some(record.epoch) {
                    return Err(MediumError::Chain {
                        rule: "epoch-regression",
                    });
                }
                if record.parent != Some(head) {
                    return Err(MediumError::Chain {
                        rule: "parent-mismatch",
                    });
                }
            }
            // Unreachable after the equality comparison above; kept as
            // a defensive stale classification.
            (observed, _) => return Ok(AppendOutcome::RejectedStaleParent { observed }),
        }
        if let Some(key) = &record.idempotency_key
            && let Some(cursor) = &self.cursor
            && cursor.seen_keys.contains(key)
        {
            return Err(MediumError::Chain {
                rule: "duplicate-idempotency-key",
            });
        }
        if let Some(cursor) = &self.cursor {
            // A dependency must name a prior revision of the
            // authoritative chain — the record-id index, never the
            // mixed reachable set (a content hash is not a revision).
            for dependency in &record.depends_on {
                if !cursor.record_ids.contains(dependency) {
                    return Err(MediumError::Chain {
                        rule: "depends-on-unresolvable",
                    });
                }
            }
        }
        self.fault_pre(FaultPoint::AfterComparison)?;

        // Step 5: write every object the candidate needs (the
        // uncertain window opens).
        let objects_started = Instant::now();
        if let Err(failure) = self.write_all_objects(objects, record_bytes, record_id) {
            return match failure {
                ObjectWriteFailure::Unknown(reason) => Ok(AppendOutcome::Unknown { reason }),
                ObjectWriteFailure::FailClosed(error) => Err(error),
            };
        }

        // Step 6: publish HEAD (the linearization point is the rename
        // inside).
        if let Err(reason) = self.publish_head(record_id) {
            return Ok(AppendOutcome::Unknown { reason });
        }

        // Step 7: post-rename metadata synchronization.
        if let Err(reason) = self.fault_window(
            FaultPoint::AfterHeadRename,
            UnknownReason::PostPublishSyncFailed,
        ) {
            return Ok(AppendOutcome::Unknown { reason });
        }
        if let Err(reason) = self.sync_root_dir() {
            return Ok(AppendOutcome::Unknown { reason });
        }
        if let Err(reason) = self.fault_window(
            FaultPoint::AfterRootDirSync,
            UnknownReason::PostPublishSyncFailed,
        ) {
            return Ok(AppendOutcome::Unknown { reason });
        }
        self.timings.object_write_sync_nanos = elapsed_nanos(objects_started);

        // The trusted cursor advances to the accepted revision.
        self.advance_cursor(record, record_id);
        Ok(AppendOutcome::Accepted {
            id: *record_id,
            tier: self.platform_tier,
        })
    }

    /// Step 5: insert-once writes for every object (contents first,
    /// the record last), then file syncs for the whole set, then the
    /// Unix objects-directory sync.
    fn write_all_objects(
        &mut self,
        objects: &[StoredObject],
        record_bytes: &[u8],
        record_id: &Hash,
    ) -> Result<(), ObjectWriteFailure> {
        // Phase A — write bytes (no syncs yet): the crash matrix's
        // "during creation" and "after creation, before
        // synchronization" boundaries.
        let mut written_first = false;
        for object in objects {
            self.insert_object(object.bytes(), &object.hash())?;
            if !written_first {
                written_first = true;
                if let Err(reason) =
                    self.fault_window(FaultPoint::MidObjectWrite, UnknownReason::ObjectWriteFailed)
                {
                    return Err(ObjectWriteFailure::Unknown(reason));
                }
            }
        }
        self.insert_object(record_bytes, record_id)?;
        if let Err(reason) =
            self.fault_window(FaultPoint::ObjectsWritten, UnknownReason::ObjectWriteFailed)
        {
            return Err(ObjectWriteFailure::Unknown(reason));
        }

        // Phase B — sync every object in the set (including
        // idempotently reused ones: a prior interrupted writer may
        // have left them unsynced). The handles open with WRITE
        // access: Windows `FlushFileBuffers` requires it (a read-only
        // handle fails with ERROR_ACCESS_DENIED), while POSIX fsync
        // accepts any descriptor — write access is the portable
        // intersection.
        let mut hashes: Vec<Hash> = objects.iter().map(StoredObject::hash).collect();
        hashes.push(*record_id);
        for hash in &hashes {
            let path = self.object_path(hash);
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|_| ObjectWriteFailure::Unknown(UnknownReason::ObjectSyncFailed))?;
            file.sync_all()
                .map_err(|_| ObjectWriteFailure::Unknown(UnknownReason::ObjectSyncFailed))?;
        }
        if let Err(reason) =
            self.fault_window(FaultPoint::ObjectsSynced, UnknownReason::ObjectSyncFailed)
        {
            return Err(ObjectWriteFailure::Unknown(reason));
        }

        // Phase C — the objects-directory sync (Unix only; §5.3.3: std
        // exposes no Windows directory flush, and the tier carries the
        // limitation).
        #[cfg(unix)]
        {
            let directory = File::open(self.root.join(OBJECTS_DIR))
                .map_err(|_| ObjectWriteFailure::Unknown(UnknownReason::ObjectSyncFailed))?;
            directory
                .sync_all()
                .map_err(|_| ObjectWriteFailure::Unknown(UnknownReason::ObjectSyncFailed))?;
        }
        if let Err(reason) = self.fault_window(
            FaultPoint::AfterObjectsDirSync,
            UnknownReason::ObjectSyncFailed,
        ) {
            return Err(ObjectWriteFailure::Unknown(reason));
        }
        Ok(())
    }

    /// Insert-once with verify-on-retry: an existing path is accepted
    /// only if its bytes hash to the expected identity; a mismatch is
    /// a corruption condition — fail-closed when the identity is
    /// chain-referenced, quarantine-then-rewrite when it is not (the
    /// §5.3.3 evidence reconciliation, never a silent overwrite).
    fn insert_object(&self, bytes: &[u8], hash: &Hash) -> Result<(), ObjectWriteFailure> {
        let path = self.object_path(hash);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(bytes)
                    .map_err(|_| ObjectWriteFailure::Unknown(UnknownReason::ObjectWriteFailed))?;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let existing = fs::read(&path)
                    .map_err(|_| ObjectWriteFailure::Unknown(UnknownReason::ObjectWriteFailed))?;
                if hash_object(&existing) != *hash {
                    let referenced = self
                        .cursor
                        .as_ref()
                        .is_some_and(|cursor| cursor.reachable.contains(hash));
                    if referenced {
                        return Err(ObjectWriteFailure::FailClosed(
                            MediumError::ReferencedObjectCorrupt { hash: *hash },
                        ));
                    }
                    self.quarantine_object(hash, "insert-once hash mismatch (unreferenced)")
                        .map_err(ObjectWriteFailure::FailClosed)?;
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(|_| {
                            ObjectWriteFailure::Unknown(UnknownReason::ObjectWriteFailed)
                        })?;
                    file.write_all(bytes).map_err(|_| {
                        ObjectWriteFailure::Unknown(UnknownReason::ObjectWriteFailed)
                    })?;
                }
            }
            Err(_) => {
                return Err(ObjectWriteFailure::Unknown(
                    UnknownReason::ObjectWriteFailed,
                ));
            }
        }
        Ok(())
    }

    /// Step 6: write `HEAD.tmp`, fsync it, rename onto `HEAD`. The
    /// rename is the protocol's linearization point.
    fn publish_head(&self, id: &Hash) -> Result<(), UnknownReason> {
        let tmp = self.root.join(HEAD_TMP);
        let bytes = {
            let mut enc = Encoder::new();
            enc.write_str(HEAD_DOMAIN);
            enc.write_u32(HEAD_VERSION);
            enc.write_bytes(id.as_bytes());
            enc.into_bytes()
        };
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)
            .map_err(|_| UnknownReason::HeadPublishFailed)?;
        file.write_all(&bytes)
            .map_err(|_| UnknownReason::HeadPublishFailed)?;
        self.fault_window(FaultPoint::HeadTmpWritten, UnknownReason::HeadPublishFailed)?;
        file.sync_all()
            .map_err(|_| UnknownReason::HeadPublishFailed)?;
        self.fault_window(FaultPoint::HeadTmpSynced, UnknownReason::HeadPublishFailed)?;
        drop(file);
        fs::rename(&tmp, self.root.join(HEAD_FILE))
            .map_err(|_| UnknownReason::HeadPublishFailed)?;
        Ok(())
    }

    /// Step 7 (Unix): the post-rename root-directory fsync.
    fn sync_root_dir(&self) -> Result<(), UnknownReason> {
        #[cfg(unix)]
        {
            let directory =
                File::open(&self.root).map_err(|_| UnknownReason::PostPublishSyncFailed)?;
            directory
                .sync_all()
                .map_err(|_| UnknownReason::PostPublishSyncFailed)?;
        }
        #[cfg(not(unix))]
        {
            // The documented std Windows sequence offers no
            // pre-rename parent-directory flush (§5.3.3); the
            // SyncTier carries the limitation honestly.
        }
        Ok(())
    }

    // -------------------------------------------------------- validation

    /// Validates the observed HEAD against the trusted cursor: O(0)
    /// record walks on the verified head, a bounded suffix walk on
    /// external divergence, a cold walk with no cursor, and fail-closed
    /// divergence when HEAD vanished or is not a descendant.
    fn validate_observed(&mut self, observed: Option<Hash>) -> Result<(), MediumError> {
        let trusted = self
            .cursor
            .as_ref()
            .map(|cursor| (cursor.head, cursor.epoch));
        match (observed, trusted) {
            (None, None) => Ok(()),
            (Some(id), Some((cursor_head, _))) if id == cursor_head => {
                self.walks.last_append_walked = 0;
                Ok(())
            }
            (Some(id), Some((trusted_head, trusted_epoch))) => {
                match self.walk_suffix(id, trusted_head, trusted_epoch) {
                    Ok(suffix) => {
                        self.walks.last_append_walked = suffix.len() as u64;
                        self.absorb_suffix(suffix, id);
                        Ok(())
                    }
                    Err(MediumError::Chain { rule: "divergence" }) => {
                        // The observed chain does not contain our
                        // verified history: a divergent rewrite —
                        // detected, never merged, fail closed.
                        Err(MediumError::Chain { rule: "divergence" })
                    }
                    Err(_) => {
                        // Sound invalidation: discard the cursor; the
                        // full walk rebuilds it or fails closed.
                        self.cursor = None;
                        self.cold_walk_from(id).map(|_| ())
                    }
                }
            }
            (Some(id), None) => self.cold_walk_from(id).map(|_| ()),
            (None, Some(_)) => Err(MediumError::Chain { rule: "divergence" }),
        }
    }

    /// Walks `target` down its parents until `until` (exclusive),
    /// verifying each record and its content references. Returns the
    /// suffix newest-first; reaching the founding record without
    /// encountering `until` is divergence. Bounded: a corrupt external
    /// chain whose parents cycle without passing through `until` fails
    /// closed (the no-hang obligation). The suffix must satisfy the
    /// same invariants the cold walk enforces: idempotency-key
    /// uniqueness, checked epoch linkage **including the trusted-cursor
    /// boundary** (the oldest suffix revision sits exactly one epoch
    /// above the trusted head), and dependency ancestry (every
    /// depends_on reference names a trusted revision or a strictly
    /// older suffix revision).
    fn walk_suffix(
        &self,
        target: Hash,
        until: Hash,
        trusted_epoch: u64,
    ) -> Result<Vec<(Hash, RevisionRecord)>, MediumError> {
        let mut suffix: Vec<(Hash, RevisionRecord)> = Vec::new();
        let mut current = target;
        let mut newer_epoch: Option<u64> = None;
        let mut visited = BTreeSet::new();
        let mut suffix_keys = BTreeSet::new();
        let (trusted_keys, trusted_record_ids) = self
            .cursor
            .as_ref()
            .map(|cursor| (cursor.seen_keys.clone(), cursor.record_ids.clone()))
            .unwrap_or_default();
        loop {
            if current == until {
                // The trusted-cursor boundary: the oldest suffix
                // revision must sit exactly one epoch above the
                // trusted head (checked arithmetic — no epoch above
                // u64::MAX can satisfy it, so the boundary can never
                // wrap).
                if let Some((_, boundary)) = suffix.last()
                    && trusted_epoch.checked_add(1) != Some(boundary.epoch)
                {
                    return Err(MediumError::Chain {
                        rule: "epoch-regression",
                    });
                }
                // Dependency ancestry, deferred until the suffix is
                // complete: newest-first, a record at index i may
                // depend only on trusted revisions (all older) or on
                // suffix revisions at indices > i (strictly older) —
                // a dependency naming a newer suffix record, itself,
                // or anything outside the authoritative history is
                // unresolvable.
                let mut positions: BTreeMap<Hash, usize> = BTreeMap::new();
                for (index, (id, _)) in suffix.iter().enumerate() {
                    positions.insert(*id, index);
                }
                for (index, (_, record)) in suffix.iter().enumerate() {
                    for dependency in &record.depends_on {
                        let resolves = trusted_record_ids.contains(dependency)
                            || positions
                                .get(dependency)
                                .is_some_and(|position| *position > index);
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
            let bytes = self.load_referenced(&current)?;
            let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
            // Checked linkage: an epoch of u64::MAX has no successor —
            // it must fail closed, never wrap.
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
                self.load_referenced(reference)?;
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

    /// Merges a verified suffix (newest-first) into the cursor and
    /// points it at the new head. Absorption runs oldest-first so the
    /// final epoch written is the newest verified epoch — for a
    /// multi-revision suffix the cursor's head, epoch, reachable set,
    /// seen-key index, and record-id index stay mutually consistent.
    fn absorb_suffix(&mut self, suffix: Vec<(Hash, RevisionRecord)>, new_head: Hash) {
        let Some(cursor) = self.cursor.as_mut() else {
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

    /// The cold founding → HEAD walk: every record hash-verified,
    /// epoch-linked with checked arithmetic, key-unique; every
    /// referenced object loaded and hash-verified; every depends_on
    /// reference resolving to a strictly earlier revision of the same
    /// authoritative history. Rebuilds the cursor and returns the
    /// summary.
    fn cold_walk_from(&mut self, head: Hash) -> Result<ChainSummary, MediumError> {
        let mut seen_ids = BTreeSet::new();
        let mut seen_keys = BTreeSet::new();
        let mut reachable = BTreeSet::new();
        let mut records = 0u64;
        let mut content_objects = 0u64;
        let mut current = head;
        let mut newer_epoch: Option<u64> = None;
        let mut head_epoch = 0u64;
        // (id, depends_on) per record in walk order (newest first) —
        // dependency ancestry is validated once the chain is complete.
        let mut walked: Vec<(Hash, Vec<Hash>)> = Vec::new();
        let (founding_id, authority, campaign_id) = loop {
            if !seen_ids.insert(current) {
                return Err(MediumError::Chain { rule: "cycle" });
            }
            let bytes = self.load_referenced(&current)?;
            let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
            if records == 0 {
                head_epoch = record.epoch;
            }
            // Checked linkage: an epoch of u64::MAX has no successor —
            // it must fail closed, never wrap.
            if let Some(epoch) = newer_epoch
                && record.epoch.checked_add(1) != Some(epoch)
            {
                return Err(MediumError::Chain {
                    rule: "epoch-regression",
                });
            }
            if let Some(key) = &record.idempotency_key
                && !seen_keys.insert(key.clone())
            {
                return Err(MediumError::Chain {
                    rule: "duplicate-idempotency-key",
                });
            }
            for reference in &record.content_refs {
                self.load_referenced(reference)?;
                if reachable.insert(*reference) {
                    content_objects += 1;
                }
            }
            reachable.insert(current);
            walked.push((current, record.depends_on.clone()));
            records += 1;
            newer_epoch = Some(record.epoch);
            match record.parent {
                Some(parent) => current = parent,
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
        // Dependency ancestry, deferred until the chain is complete:
        // the walk runs newest-first, so a record at position i can
        // only depend on revisions at positions > i (strictly earlier).
        // A dependency naming a nonexistent revision, a content hash,
        // itself, or any later revision is unresolvable — hash
        // consistency alone never makes a chain semantically valid.
        let mut positions: BTreeMap<Hash, usize> = BTreeMap::new();
        for (index, (id, _)) in walked.iter().enumerate() {
            positions.insert(*id, index);
        }
        for (index, (_, dependencies)) in walked.iter().enumerate() {
            for dependency in dependencies {
                if !positions
                    .get(dependency)
                    .is_some_and(|position| *position > index)
                {
                    return Err(MediumError::Chain {
                        rule: "depends-on-unresolvable",
                    });
                }
            }
        }
        self.walks.cold_walk_records = records;
        self.cursor = Some(Cursor {
            head,
            epoch: head_epoch,
            seen_keys,
            reachable,
            record_ids: seen_ids,
            founding: founding_id,
            authority,
            campaign_id: campaign_id.clone(),
            content_objects,
        });
        Ok(ChainSummary {
            head,
            height: head_epoch,
            founding: founding_id,
            authority,
            campaign_id,
            records,
            content_objects,
        })
    }

    /// Advances the cursor over an accepted append (O(1) work).
    fn advance_cursor(&mut self, record: &RevisionRecord, id: &Hash) {
        let refs = &record.content_refs;
        match self.cursor.as_mut() {
            None => {
                let RecordKind::Founding {
                    authority,
                    campaign_id,
                } = &record.kind
                else {
                    // A regular append always has a cursor (validated).
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
                self.cursor = Some(Cursor {
                    head: *id,
                    epoch: record.epoch,
                    seen_keys,
                    reachable,
                    record_ids,
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

    // ---------------------------------------------------- orphan protocol

    /// Moves an object into quarantine with its bytes and a reason
    /// file — never deletes; idempotent when already moved.
    fn quarantine_object(&self, hash: &Hash, reason: &str) -> Result<(), MediumError> {
        self.quarantine_named(&hash.hex(), reason)
    }

    /// Incident-suffix ceiling for collision-safe quarantine placement.
    const QUARANTINE_NAME_ATTEMPTS: u32 = 10_000;

    fn quarantine_named(&self, name: &str, reason: &str) -> Result<(), MediumError> {
        let objects = self.root.join(OBJECTS_DIR);
        let quarantine = self.root.join(QUARANTINE_DIR);
        let source = objects.join(name);
        // Collision-safe across BOTH namespaces — evidence and
        // metadata: an incident is placed only where its evidence
        // path AND its .reason note path are both free, whatever
        // those paths currently hold (a preserved evidence file
        // named `x.reason` is exactly as protected as a note file),
        // and the note is created with create-new semantics so no
        // quarantine operation can ever overwrite an existing file.
        // The first incident keeps the plain name; later incidents
        // append .1, .2, … — preserved evidence and provenance are
        // never destroyed. All quarantine sites run under HEAD.lock,
        // so the exists-then-place sequence is serialized; an
        // interrupted move leaves the source in place (idempotent on
        // retry), and an interrupted note-write leaves the bytes
        // preserved under their incident name.
        let mut attempt = 0u32;
        let (target, note) = loop {
            let suffix = if attempt == 0 {
                String::new()
            } else {
                format!(".{attempt}")
            };
            let target = quarantine.join(format!("{name}{suffix}"));
            let note = quarantine.join(format!("{name}{suffix}.reason"));
            if !target.exists() && !note.exists() {
                break (target, note);
            }
            attempt += 1;
            if attempt > Self::QUARANTINE_NAME_ATTEMPTS {
                return Err(io_err(
                    "quarantine collision limit",
                    &target,
                    io::Error::other("too many quarantine incidents under one name"),
                ));
            }
        };
        match fs::rename(&source, &target) {
            Ok(()) => {
                // Non-overwriting provenance creation: even an
                // overlooked collision fails loudly rather than
                // destroying evidence.
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&note)
                    .map_err(|e| io_err("create quarantine reason", &note, e))?;
                file.write_all(reason.as_bytes())
                    .map_err(|e| io_err("write quarantine reason", &note, e))?;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err("quarantine object", &source, e)),
        }
    }

    /// The session-open sweep: under the lock, every `objects/` entry
    /// not reachable from HEAD is quarantined (junk names included);
    /// reachable objects were hash-verified during the cold walk.
    fn sweep_unreferenced(&mut self) -> Result<u64, MediumError> {
        let reachable = self
            .cursor
            .as_ref()
            .map(|cursor| cursor.reachable.clone())
            .unwrap_or_default();
        let mut quarantined = 0u64;
        let dir = self.root.join(OBJECTS_DIR);
        for entry in fs::read_dir(&dir).map_err(|e| io_err("read objects dir", &dir, e))? {
            let entry = entry.map_err(|e| io_err("read objects entry", &dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let referenced = Hash::from_hex(&name).is_some_and(|hash| reachable.contains(&hash));
            if !referenced {
                self.quarantine_named(&name, "sweep-unreferenced")?;
                quarantined += 1;
            }
        }
        Ok(quarantined)
    }

    // ------------------------------------------------------ reconciliation

    fn reconcile_locked(&mut self, candidate: &Hash) -> Result<Reconciliation, MediumError> {
        if let Err(error) = self.fault_pre(FaultPoint::ReconcileLocked) {
            return Ok(Reconciliation::Unverifiable {
                reason: error.to_string(),
            });
        }
        let observed = self.read_head_file()?;
        if let Err(error) = self.fault_pre(FaultPoint::ReconcileHeadRead) {
            return Ok(Reconciliation::Unverifiable {
                reason: error.to_string(),
            });
        }
        let mut walked = 0u64;
        let result = match observed {
            Some(head) => {
                // Full verification before any classification: the
                // cold walk establishes a fully valid authoritative
                // chain — the candidate's own revision, its content,
                // its dependencies, and every descendant are
                // hash-verified and coherence-checked. `Installed` is
                // only ever returned when the candidate's presence in
                // that verified chain is established (HEAD itself, or
                // a member of the walked revision-id set); a matching
                // parent hash alone is never proof. Corruption
                // anywhere fails closed (a typed error), never a
                // false confirmation.
                self.cursor = None;
                let summary = self.cold_walk_from(head)?;
                walked = summary.records;
                let installed = head == *candidate
                    || self
                        .cursor
                        .as_ref()
                        .is_some_and(|cursor| cursor.record_ids.contains(candidate));
                if installed {
                    Reconciliation::Installed { id: *candidate }
                } else {
                    // Never installed in a verified chain: the
                    // candidate object, if present, is unreferenced
                    // evidence of the ambiguous attempt — quarantine
                    // it (never delete), then report retry-safe
                    // absence.
                    if self.object_path(candidate).exists() {
                        self.quarantine_object(
                            candidate,
                            "reconcile: candidate not installed (unreferenced)",
                        )?;
                    }
                    Reconciliation::NotInstalled { head: Some(head) }
                }
            }
            None => {
                // No authoritative history exists (never founded, or
                // HEAD vanished): nothing can be verified installed —
                // the candidate object, if present, is unreferenced
                // evidence; quarantine it and report retry-safe
                // absence.
                if self.object_path(candidate).exists() {
                    self.quarantine_object(
                        candidate,
                        "reconcile: candidate not installed (unreferenced)",
                    )?;
                }
                Reconciliation::NotInstalled { head: None }
            }
        };
        self.walks.last_reconcile_walked = walked;
        Ok(result)
    }
}

impl CanonicalMedium for LocalFileMedium {
    fn head(&mut self) -> Result<Option<Hash>, MediumError> {
        self.lock()?;
        // Capture-then-unlock (the reviewed open() discipline): no
        // early return between lock and unlock — an error path must
        // never leak the OS lock while the medium stays alive.
        let outcome = self
            .read_head_file()
            .and_then(|observed| self.validate_observed(observed).map(|()| observed));
        let _ = self.unlock();
        outcome
    }

    fn append(
        &mut self,
        record: &RevisionRecord,
        objects: &[StoredObject],
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        let encode_started = Instant::now();
        // Structural validation (pre-window; a failure is a clean
        // error — nothing has been written): the record must be
        // canonically representable and reference exactly the supplied
        // object set.
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
        self.timings.encode_nanos = elapsed_nanos(encode_started);

        // Step 1: acquire the OS-managed coordination.
        self.fault_pre(FaultPoint::BeforeLock)?;
        self.lock()?;
        let outcome =
            self.append_locked(record, objects, expected_parent, &record_bytes, &record_id);
        // Step 8: release after the defined outcome. An unlock failure
        // does not alter a reached outcome — the OS releases ownership
        // at handle close or process death either way.
        let _ = self.unlock();
        outcome
    }

    fn read_record(&mut self, id: &Hash) -> Result<RevisionRecord, MediumError> {
        self.lock()?;
        let decoded = self
            .load_object(id)
            .and_then(|bytes| RevisionRecord::decode(&bytes).map_err(MediumError::Format));
        let _ = self.unlock();
        decoded
    }

    fn read_object(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
        self.lock()?;
        let bytes = self.load_object(hash);
        let _ = self.unlock();
        bytes
    }

    fn validate(&mut self) -> Result<ChainSummary, MediumError> {
        self.lock()?;
        // Capture-then-unlock (the reviewed open() discipline): no
        // early return between lock and unlock — an error path must
        // never leak the OS lock while the medium stays alive.
        let result = match self.read_head_file() {
            Ok(Some(id)) => {
                self.cursor = None;
                self.cold_walk_from(id)
            }
            Ok(None) => Err(MediumError::Chain { rule: "unfounded" }),
            Err(error) => Err(error),
        };
        let _ = self.unlock();
        result
    }

    fn reconcile_unknown(&mut self, candidate: &Hash) -> Result<Reconciliation, MediumError> {
        let started = Instant::now();
        self.lock()?;
        let result = self.reconcile_locked(candidate);
        let _ = self.unlock();
        self.timings.reconcile_nanos = elapsed_nanos(started);
        result
    }

    fn durability_tier(&self) -> SyncTier {
        self.platform_tier
    }
}

/// Reads and decodes the authoritative HEAD file at a store root —
/// the raw, cursor-free read (the offline migration's cutover
/// recheck under its own held lock, PP-B1 §6.1).
pub(crate) fn read_head_at(root: &Path) -> Result<Option<Hash>, MediumError> {
    let path = root.join(HEAD_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err("read HEAD", &path, e)),
    };
    let corrupt = |error: engine_core::SaveError| MediumError::CorruptHead {
        detail: error.to_string(),
    };
    let mut dec = Decoder::new(&bytes);
    dec.read_domain(HEAD_DOMAIN).map_err(corrupt)?;
    let version = dec.read_u32().map_err(corrupt)?;
    if version != HEAD_VERSION {
        return Err(MediumError::CorruptHead {
            detail: format!("unsupported HEAD version {version}"),
        });
    }
    let hash_bytes = dec.read_bytes(Hash::WIDTH, "head").map_err(corrupt)?;
    let id = Hash::from_bytes(hash_bytes).ok_or_else(|| MediumError::CorruptHead {
        detail: "hash width".into(),
    })?;
    dec.finish().map_err(corrupt)?;
    Ok(Some(id))
}

fn platform_tier() -> SyncTier {
    if cfg!(windows) {
        SyncTier::ProcessCrashOnly
    } else {
        SyncTier::FullPosixSequence
    }
}

fn io_err(op: &'static str, path: &Path, source: io::Error) -> MediumError {
    MediumError::Io {
        op,
        path: path.display().to_string(),
        source,
    }
}

fn elapsed_nanos(from: Instant) -> u64 {
    u64::try_from(from.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
