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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use engine_core::wire::MAX_BYTE_LEN;
use engine_core::{Decoder, Encoder};

use crate::checkpoint::{
    CHECKPOINT_METADATA_DOMAIN, CheckpointMetadata, ReclaimedIndexEntry, peek_object_domain,
};
use crate::composite_manifest;
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
    /// The reclaim-indexed revisions (identity → epoch) from every
    /// on-chain MK's index whose records are genuinely absent — the
    /// historical membership evidence (PP-B3's condition 2: a
    /// reclaimed revision's key-dedup and depends_on resolution
    /// survive its deletion). The observational counts (the MK list,
    /// the reclaimed totals) live on the [`ChainSummary`] the walk
    /// returns, never on the cursor.
    reclaimed: BTreeMap<Hash, u64>,
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
    #[allow(clippy::too_many_arguments)]
    fn append_locked(
        &mut self,
        record: &RevisionRecord,
        objects: &[StoredObject],
        expected_parent: Option<Hash>,
        record_bytes: &[u8],
        record_id: &Hash,
        composite: bool,
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
            // mixed reachable set (a content hash is not a revision)
            // — or a reclaim-indexed revision with its indexed epoch
            // strictly below the candidate's (PP-B3's condition 2).
            for dependency in &record.depends_on {
                let resolves = cursor.record_ids.contains(dependency)
                    || cursor
                        .reclaimed
                        .get(dependency)
                        .is_some_and(|&dep_epoch| dep_epoch < record.epoch);
                if !resolves {
                    return Err(MediumError::Chain {
                        rule: "depends-on-unresolvable",
                    });
                }
            }
        }
        self.fault_pre(FaultPoint::AfterComparison)?;

        // The K2-C2A PUBLICATION FENCE: revalidate every REUSED
        // composite dependency under HEAD.lock, immediately before
        // the first mutating write — the admission's caller-side
        // reads are advisory; a dependency that vanished (a sweep's
        // quarantine move, a committed reclamation delete) or
        // changed between the admission and this publication
        // REFUSES typed, with nothing mutated. The loader is the
        // lock-held internal hash-verifying read the walks use —
        // no public storage operation, no nested coordination.
        if composite {
            let supplied: BTreeSet<Hash> = objects.iter().map(StoredObject::hash).collect();
            crate::composite_append::fence_reused_dependencies(
                objects[0].bytes(),
                &supplied,
                |identity| self.load_referenced(identity),
            )?;
        }

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
        // The K2-C1 corrective: a composite append advances the
        // cursor with the VERIFIED closure (the manifest's
        // identities — the admission already verified every chunk;
        // the manifest re-decode here is the same bounded object).
        if composite {
            let manifest =
                composite_manifest::decode(objects[0].bytes()).map_err(MediumError::Format)?;
            self.advance_cursor_with_closure(
                record,
                record_id,
                Some(&manifest.closure_identities()),
            );
        } else {
            self.advance_cursor(record, record_id);
        }
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
                        self.absorb_suffix(suffix, id)?;
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
    /// depends_on reference names a trusted revision, a strictly
    /// older suffix revision, or a reclaim-indexed revision with its
    /// indexed epoch strictly earlier). A suffix may pass a foreign
    /// MK — its record and metadata object validate like any other,
    /// but its index is not absorbed into this handle's cursor: the
    /// membership universe is rebuilt by the cold walk at the next
    /// open (under the single-writer discipline a mid-handle foreign
    /// MK is one this process published through another,
    /// already-closed handle).
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
        let (trusted_keys, trusted_record_ids, reclaimed) = self
            .cursor
            .as_ref()
            .map(|cursor| {
                (
                    cursor.seen_keys.clone(),
                    cursor.record_ids.clone(),
                    cursor.reclaimed.clone(),
                )
            })
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
    fn absorb_suffix(
        &mut self,
        suffix: Vec<(Hash, RevisionRecord)>,
        new_head: Hash,
    ) -> Result<(), MediumError> {
        // The R10 corrective: precompute each suffix record's
        // VERIFIED composite closure (if its first reference is a
        // /2 root) BEFORE the cursor borrow — every failure
        // PROPAGATES (never silently treated as an absent closure):
        // the hash-verified load, the domain peek, the strict
        // manifest decode, and EVERY dependency's full envelope
        // verification (the same verify_chunk_dependency the cold
        // walk uses). A missing, corrupt, or malformed transitive
        // chunk refuses the suffix absorption — the trusted cursor
        // does NOT advance past unverifiable composite state.
        let mut closures: Vec<(Hash, RevisionRecord, Option<Vec<Hash>>)> = Vec::new();
        for (id, record) in suffix.into_iter() {
            let mut closure = None;
            if let Some(root) = record.content_refs.first() {
                let bytes = self.load_referenced(root)?;
                // R13: the domain peek PROPAGATES its failure — a
                // malformed leading prefix is a typed fail-closed
                // refusal, never silently treated as an unrelated
                // object. The recognized-but-unsupported future
                // family member (e.g. /3) follows the forward-
                // compatibility semantics: it passes the peek (a
                // valid domain string), does not match /2, and is
                // carried as a non-composite reference — the
                // recovery-level fail-closed (R11's regression)
                // handles it.
                let domain = peek_object_domain(&bytes).map_err(MediumError::Format)?;
                if domain == composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN {
                    let manifest =
                        composite_manifest::decode(&bytes).map_err(MediumError::Format)?;
                    let mut identities = Vec::with_capacity(manifest.entries.len());
                    for entry in &manifest.entries {
                        let chunk_bytes = self.load_referenced(&entry.identity)?;
                        crate::composite_append::verify_chunk_dependency(
                            &chunk_bytes,
                            &entry.identity,
                            &manifest,
                        )
                        .map_err(|_error| MediumError::Chain {
                            rule: "composite-suffix-closure-verification",
                        })?;
                        identities.push(entry.identity);
                    }
                    closure = Some(identities);
                }
            }
            closures.push((id, record, closure));
        }
        let Some(cursor) = self.cursor.as_mut() else {
            return Ok(());
        };
        for (id, record, closure) in closures.into_iter().rev() {
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
            if let Some(closure) = closure {
                for identity in closure {
                    if cursor.reachable.insert(identity) {
                        cursor.content_objects += 1;
                    }
                }
            }
            cursor.epoch = record.epoch;
        }
        cursor.head = new_head;
        Ok(())
    }

    /// The cold founding → HEAD walk: every record hash-verified,
    /// epoch-linked with checked arithmetic, key-unique; every
    /// referenced object loaded and hash-verified; every depends_on
    /// reference resolving to a strictly earlier revision of the same
    /// authoritative history (retained, or reclaim-indexed with its
    /// indexed epoch). Rebuilds the cursor and returns the summary.
    ///
    /// **The compacted mode (PP-B3):** an absent parent is a
    /// legitimately-reclaimed revision only when the most recent
    /// on-chain MK's skip table covers the gap — a skip for exactly
    /// the record whose parent is absent (or that MK's own self
    /// entry), jumping to the nearest retained ancestor under the
    /// gap-aware strictly-earlier epoch linkage. An uncovered absence
    /// is hard corruption — fail closed, never assumed reclamation.
    fn cold_walk_from(&mut self, head: Hash) -> Result<ChainSummary, MediumError> {
        let (summary, cursor) = self.cold_walk_excluding(head, &BTreeSet::new())?;
        self.walks.cold_walk_records = summary.records;
        self.cursor = Some(cursor);
        Ok(summary)
    }

    /// The authenticated virtual-exclusion survivor walk (K2-C2B
    /// P2): the cold walk with the AUDITED retiring-record set
    /// treated as virtually absent — a physically present record in
    /// the exclusion routes through the SAME authenticated skip
    /// jump an absent one takes (the covering MK's table, the
    /// gap-aware epoch linkage), and the MK-index staging
    /// automatically carries the excluded records into the
    /// reclaimed-membership universe (preserving depends_on
    /// resolution and key history exactly as physical reclamation
    /// does). **P2-F1: the simulated post-retirement view is
    /// returned as DATA — the authoritative trusted cursor, the
    /// walk counters, and every other provider state are untouched
    /// (the walk is a pure `&self` computation), so an unsuccessful
    /// walk, an authorization refusal, or a resource limit leaves
    /// the provider's ordinary state exactly as it was, and no
    /// append, validation, or sweep ever observes the simulated
    /// view.** The caller decides whether and when to establish a
    /// cursor consistent with the post-deletion physical state.
    fn cold_walk_excluding(
        &self,
        head: Hash,
        exclude: &BTreeSet<Hash>,
    ) -> Result<(ChainSummary, Cursor), MediumError> {
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
        // records are still present and retained; only genuinely
        // absent entries are reclaimed members).
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
            let bytes = self.load_referenced(&current)?;
            let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
            if records == 0 {
                head_epoch = record.epoch;
            }
            // Checked linkage: an epoch of u64::MAX has no successor —
            // it must fail closed, never wrap. Across a skip-table
            // jump the linkage is gap-aware instead: the target must
            // sit STRICTLY below the pre-gap record (any gap size ≥ 1
            // — the reclaimed span was removed whole).
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
                let bytes = self.load_referenced(reference)?;
                if position == 0 {
                    first_object = Some(bytes);
                }
                if reachable.insert(*reference) {
                    content_objects += 1;
                }
            }
            // The MK recognition: a record whose first content object
            // carries the checkpoint-metadata domain — its index
            // joins the membership universe (staged for the post-walk
            // merge) and its skip table becomes the gap cover for
            // everything below it.
            if let Some(bytes) = first_object {
                let domain = peek_object_domain(&bytes).map_err(MediumError::Format)?;
                // The K2-C1 corrective's VERIFIED composite-closure
                // reachability: a /2 root's chunks are transitively
                // referenced through its manifest — the walk proves
                // EVERY dependency (the hash-verified read, the full
                // envelope verification against the manifest entry)
                // before it joins the reachable set. A missing,
                // corrupt, or malformed chunk fails the walk closed;
                // nothing is silently ignored.
                if domain == composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN {
                    let manifest =
                        composite_manifest::decode(&bytes).map_err(MediumError::Format)?;
                    for entry in &manifest.entries {
                        let chunk_bytes = self.load_referenced(&entry.identity)?;
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
                    let metadata =
                        CheckpointMetadata::decode(&bytes).map_err(MediumError::Format)?;
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
                    // The compacted walk: an absent parent is a gap —
                    // legitimate only under the covering skip table;
                    // anything else is hard corruption (fail closed,
                    // never assumed reclamation). The K2-C2B P2
                    // exclusion: an AUDITED retiring record routes
                    // through the same authenticated jump (the
                    // directive's chain-verified membership is the
                    // only virtual-absence authority).
                    if self.object_path(&parent).is_file() && !exclude.contains(&parent) {
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
        // The membership universe merge: only genuinely-absent
        // entries are reclaimed members — pre-reclamation the range
        // records are still present, retained, and already accounted
        // (their index entries are the evidence for the FUTURE
        // reclamation, never double-counted).
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
        // Dependency ancestry, deferred until the chain is complete:
        // the walk runs newest-first, so a record at position i can
        // only depend on revisions at positions > i (strictly
        // earlier), or on a reclaim-indexed revision with its indexed
        // epoch strictly below the referencing record's. A dependency
        // naming a nonexistent revision, a content hash, itself, or
        // any later revision is unresolvable — hash consistency
        // alone never makes a chain semantically valid.
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
        let cursor = Cursor {
            head,
            epoch: head_epoch,
            seen_keys,
            reachable,
            record_ids: seen_ids,
            reclaimed,
            founding: founding_id,
            authority,
            campaign_id: campaign_id.clone(),
            content_objects,
        };
        Ok((
            ChainSummary {
                head,
                height: head_epoch,
                founding: founding_id,
                authority,
                campaign_id,
                records,
                content_objects,
                checkpoints,
                reclaimed_records,
                reclaimed_keys,
            },
            cursor,
        ))
    }

    /// Advances the cursor over an accepted append (O(1) work).
    fn advance_cursor(&mut self, record: &RevisionRecord, id: &Hash) {
        self.advance_cursor_with_closure(record, id, None);
    }

    /// The cursor advance with the optional VERIFIED composite
    /// closure (the K2-C1 corrective): a composite append's chunks
    /// join the reachable set through the admission-verified
    /// manifest, keeping the hot cursor and the cold walk on the
    /// SAME definition of reachability.
    fn advance_cursor_with_closure(
        &mut self,
        record: &RevisionRecord,
        id: &Hash,
        composite_closure: Option<&[Hash]>,
    ) {
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
                if let Some(closure) = composite_closure {
                    for identity in closure {
                        if reachable.insert(*identity) {
                            content_objects += 1;
                        }
                    }
                }
                self.cursor = Some(Cursor {
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

    fn append_admitted(
        &mut self,
        admitted: crate::medium::AdmittedAppend,
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        let encode_started = Instant::now();
        let (record, objects, composite) = admitted.parts();
        // The set rule BY KIND: the generic form keeps the exact
        // supplied==referenced equality (unchanged); the composite
        // form carries the ROOT-ONLY record with the
        // admission-verified extras (the root plus the new chunks —
        // the record's single reference IS the root, and every
        // extra was verified against the root manifest by the
        // in-crate admission that built the receipt).
        let record = record.clone();
        let objects: Vec<StoredObject> = objects.to_vec();
        if composite
            && (record.content_refs.len() != 1
                || objects.is_empty()
                || objects[0].hash() != record.content_refs[0])
        {
            return Err(MediumError::Chain {
                rule: "composite-receipt-shape",
            });
        }
        // Structural validation (pre-window; a failure is a clean
        // error — nothing has been written): the record must be
        // canonically representable and reference exactly the supplied
        // object set.
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
        self.timings.encode_nanos = elapsed_nanos(encode_started);

        // Step 1: acquire the OS-managed coordination.
        self.fault_pre(FaultPoint::BeforeLock)?;
        self.lock()?;
        let outcome = self.append_locked(
            &record,
            &objects,
            expected_parent,
            &record_bytes,
            &record_id,
            composite,
        );
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

    fn reclaim(
        &mut self,
        directive: &crate::reclaim::ReclaimDirective,
    ) -> Result<crate::reclaim::ReclaimOutcome, MediumError> {
        // THE TRUSTED BOUNDARY (K2-C2B P2): the exclusive HEAD.lock
        // covers the authentication, the partition, and the
        // deletion — no authoritative state can change between the
        // decision and its destructive consequences. The
        // capture-then-unlock discipline: no early return leaks the
        // lock.
        self.lock()?;
        let outcome = self.reclaim_authorized_locked(directive);
        let _ = self.unlock();
        outcome
    }

    fn reclaim_pending(
        &mut self,
        directive: &crate::reclaim::ReclaimDirective,
    ) -> Result<bool, MediumError> {
        // The Mandatory-only probe: the same authentication and
        // partition (lock-free — the advisory read), the mandatory
        // identities across their authenticated storage forms.
        let (ledger, _metadata, head) = self.authenticate_audit_locked(&directive.audit_record)?;
        if ledger.units != directive.units {
            return Err(MediumError::Chain {
                rule: "reclaim/directive-units-mismatch",
            });
        }
        let survivors = self.survivor_set_locked(head, &ledger)?;
        let partition = crate::reclaim::partition_reclaim(&ledger.units, &survivors)?;
        for identity in &partition.mandatory {
            // (E5) The normal-path probe matches the deletion
            // discipline: hash-verify the bytes — a mismatched file
            // at an audited name is PRESERVED evidence, never
            // authenticated unfinished retirement (never pending).
            // The read itself is CLASSIFIED, never swallowed: a
            // genuinely absent path is the absence; a failed read
            // (permissions, transient I/O, an inaccessible path) is
            // a typed refusal — an unverifiable observation never
            // silently contributes to a completed result.
            match fs::read(self.object_path(identity)) {
                Ok(bytes) => {
                    if hash_object(&bytes) == *identity {
                        return Ok(true);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(io_err("probe object", &self.object_path(identity), e));
                }
            }
            for path in self.quarantine_family(identity)? {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name.ends_with(".reason") {
                    continue;
                }
                if path.is_file() {
                    let bytes = fs::read(&path)
                        .map_err(|e| io_err("probe quarantined evidence", &path, e))?;
                    if hash_object(&bytes) == *identity {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

/// The trusted reclamation internals (K2-C2B P2) — all under the
/// already-held HEAD.lock, all using the INTERNAL hash-verified
/// read primitives (the public reads' own lock/unlock pairs would
/// break this boundary).
impl LocalFileMedium {
    /// The canonical audit authentication under the boundary: the
    /// gap-aware descent from the authoritative HEAD over the
    /// internal reads — the exact audit identity (the hash-verified
    /// load BY identity), the MK derived from the decoded ledger,
    /// the deterministic key, the requested MK's metadata bound by
    /// revision identity, the SK/digest/seed bindings, and the exact
    /// units-vs-index membership. The RECOVERY-SAFE form: the
    /// per-unit manifest reread is NOT here (the main flow applies
    /// it with the resume tolerance — an absent retiring record is
    /// accounted for by the durable audit).
    fn authenticate_audit_locked(
        &self,
        audit_record: &Hash,
    ) -> Result<(crate::reclaim::ReclamationLedger, CheckpointMetadata, Hash), MediumError> {
        const MAX_DESCENT: u64 = 1 << 20;
        let mut current = self
            .read_head_file()?
            .ok_or(MediumError::Chain { rule: "unfounded" })?;
        let head = current;
        let mut ledger: Option<crate::reclaim::ReclamationLedger> = None;
        let mut metadata: Option<CheckpointMetadata> = None;
        let mut skip_table: Option<CheckpointMetadata> = None;
        for _ in 0..MAX_DESCENT {
            let bytes = self.load_object(&current)?;
            let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
            let parent = record.parent;
            if let Some(object) = record.content_refs.first().copied() {
                let object_bytes = self.load_object(&object)?;
                let domain = StoredObject::new(object_bytes.clone())?.domain().to_owned();
                if domain == CHECKPOINT_METADATA_DOMAIN {
                    let decoded =
                        CheckpointMetadata::decode(&object_bytes).map_err(MediumError::Format)?;
                    skip_table = Some(decoded.clone());
                    if let Some(ledger) = ledger.as_ref()
                        && current == ledger.mk
                        && decoded.sk != parent.unwrap_or(Hash::ZERO)
                    {
                        return Err(MediumError::Chain {
                            rule: "reclaim/mk-binding-mismatch",
                        });
                    }
                    if let Some(ledger) = ledger.as_ref()
                        && current == ledger.mk
                    {
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
            if self.object_path(&parent).is_file() {
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
        if index.len() != metadata.index.len()
            || units.len() != ledger.units.len()
            || index != units
        {
            return Err(MediumError::Chain {
                rule: "reclaim/units-index-mismatch",
            });
        }
        Ok((ledger, metadata, head))
    }

    /// The survivor set under the boundary: the exclusion walk over
    /// the audited retiring records (pure `&self` — the authoritative
    /// cursor is never touched).
    fn survivor_set_locked(
        &self,
        head: Hash,
        ledger: &crate::reclaim::ReclamationLedger,
    ) -> Result<BTreeSet<Hash>, MediumError> {
        let exclusion: BTreeSet<Hash> = ledger.units.iter().map(|unit| unit.record).collect();
        let (_summary, simulated) = self.cold_walk_excluding(head, &exclusion)?;
        Ok(simulated.reachable)
    }

    /// The W1 quarantine name-family discovery: ONE bounded read_dir
    /// pass collecting every entry in the identity's family (the
    /// plain name, the .reason sidecar, the suffixed incidents and
    /// their sidecars) — the directory listing is the truth; sparse
    /// suffixes cannot be missed.
    fn quarantine_family(&self, identity: &Hash) -> Result<Vec<PathBuf>, MediumError> {
        let dir = self.root.join(QUARANTINE_DIR);
        let plain = identity.hex();
        let prefix = format!("{plain}.");
        let mut family = Vec::new();
        let entries = fs::read_dir(&dir).map_err(|e| io_err("read quarantine dir", &dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_err("read quarantine entry", &dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_family = name == plain
                || name == format!("{plain}.reason")
                || (name.starts_with(&prefix) && {
                    let rest = &name[plain.len() + 1..];
                    let digits = rest.strip_suffix(".reason").unwrap_or(rest);
                    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
                });
            if is_family {
                family.push(dir.join(name));
            }
        }
        Ok(family)
    }

    /// The trusted two-pass execution under the boundary. Every
    /// authorization refusal occurs BEFORE any mutation; the passes
    /// are idempotent and restart-safe (the resume from the durable
    /// audit: an absent retiring record's manifest reread is skipped
    /// — the audit accounts for it; an absent SURVIVOR record fails
    /// the walk closed).
    fn reclaim_authorized_locked(
        &mut self,
        directive: &crate::reclaim::ReclaimDirective,
    ) -> Result<crate::reclaim::ReclaimOutcome, MediumError> {
        // (1-3) The authentication and the directive cross-check.
        let (ledger, metadata, head) = self.authenticate_audit_locked(&directive.audit_record)?;
        if ledger.units != directive.units {
            return Err(MediumError::Chain {
                rule: "reclaim/directive-units-mismatch",
            });
        }
        // (E2) The newest-first retirement order: the persisted unit
        // sequence must be strictly descending by indexed epoch —
        // the provider never trusts the caller's construction.
        crate::reclaim::verify_newest_first_order(&ledger.units, &metadata)?;
        // (4) The manifest reread with the resume tolerance: a
        // PRESENT retiring record's objects must equal its actual
        // content references; an ABSENT one is the protocol-accounted
        // resume — but ONLY in the deletion-prefix shape. The (E1)
        // recovery boundary: this provider's pass 1 deletes
        // newest-first, so a genuine interruption leaves the absent
        // set as a PREFIX of the unit order (an absent unit is never
        // followed by a present one). Any other absence pattern is
        // evidence of missing storage, never proof of a prior
        // authorized deletion — the fabricated-audit-with-missing-
        // data shape fails closed. The prefix-shaped acceptance is
        // the UNRESOLVED recovery-policy question (Option A/B/C —
        // the owner decision is PENDING; no option is ratified): the
        // documented, narrower trust under which the range digest is
        // NOT proven for absent members.
        let mut present_seen = false;
        for unit in &ledger.units {
            let record_path = self.object_path(&unit.record);
            if record_path.is_file() {
                present_seen = true;
                let bytes = self.load_object(&unit.record)?;
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
        // (5) The survivor partition under the same boundary — and
        // the E1 range commitments: the founding-derived seed
        // (ALWAYS verifiable — the founding is permanent) and, on
        // the FIRST execution (every range publication present),
        // the recomputed range digest over the canonical bytes (a
        // fabricated-but-matching stored commitment refuses; the
        // interrupted resume with absent publications trusts the
        // durable audit — the ratified recovery contract).
        let (_summary, simulated) =
            self.cold_walk_excluding(head, &ledger.units.iter().map(|unit| unit.record).collect())?;
        crate::reclaim::verify_founding_seed(&metadata, &simulated.founding)?;
        let all_present = ledger
            .units
            .iter()
            .all(|unit| unit.record == ledger.sk || self.object_path(&unit.record).is_file());
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
                range_bytes.push(self.load_object(&record)?);
            }
            let recomputed = crate::checkpoint::chain_range_digest(&metadata.seed, &range_bytes);
            if recomputed != ledger.range_digest {
                return Err(MediumError::Chain {
                    rule: "reclaim/range-digest-mismatch",
                });
            }
        }
        let survivors = simulated.reachable;
        let mut partition = crate::reclaim::partition_reclaim(&ledger.units, &survivors)?;
        let mut removed = 0u64;
        // (E4) The cursor lifecycle: once the mutation phase begins,
        // the pre-retirement cursor is NEVER authoritative — the
        // invalidation precedes pass 1 (an interrupted mutation
        // leaves the safe invalidated state; the next operation
        // rebuilds via the authenticated walk).
        self.cursor = None;
        // (6) PASS 1 — the retiring records, newest-first (the
        // ledger's unit order — the crossing's Phase A collection
        // order; never the index's hash order), idempotent. The
        // genuine crash seams (the real-kill evidence): the
        // mid-record-pass window after each record deletion, and the
        // last-record/before-objects window — the two-pass
        // schedule's interruption boundaries.
        for (index, unit) in ledger.units.iter().enumerate() {
            let record_path = self.object_path(&unit.record);
            if record_path.is_file() {
                fs::remove_file(&record_path)
                    .map_err(|e| io_err("reclaim record", &record_path, e))?;
                removed += 1;
            }
            reclaim_mid_unit_seam();
            if index + 1 == ledger.units.len() {
                reclaim_last_unit_seam();
            }
        }
        // (7) PASS 2 — the deduplicated MANDATORY objects only (the
        // partition's output is already deduplicated); the deferred
        // identities are never touched. The W1 discipline on every
        // form: hash-authenticate before deleting; preserve
        // mismatched or unrelated evidence; the sidecars follow
        // their authenticated evidence (an orphaned sidecar is the
        // residue of THIS identity's cleanup).
        for identity in &partition.mandatory {
            let object_path = self.object_path(identity);
            if object_path.is_file() {
                let bytes = fs::read(&object_path)
                    .map_err(|e| io_err("reclaim object read", &object_path, e))?;
                if hash_object(&bytes) == *identity {
                    fs::remove_file(&object_path)
                        .map_err(|e| io_err("reclaim object", &object_path, e))?;
                    removed += 1;
                }
                // A mismatched file at the audited name is preserved
                // evidence — never removed on a filename match.
            }
            for path in self.quarantine_family(identity)? {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name.ends_with(".reason") {
                    // A sidecar: the residue rule — it follows its
                    // evidence; an orphan (the evidence absent or
                    // just removed) is THIS cleanup's residue. The
                    // is-file tolerance: the evidence's own
                    // companion removal may have already removed
                    // this sidecar within the same family pass
                    // (idempotent convergence).
                    let evidence = path.with_file_name(name.trim_end_matches(".reason"));
                    if !evidence.is_file() && path.is_file() {
                        fs::remove_file(&path)
                            .map_err(|e| io_err("reclaim sidecar residue", &path, e))?;
                        removed += 1;
                    }
                } else if path.is_file() {
                    // An evidence file: hash-authenticate BEFORE
                    // deleting; a mismatch is preserved suspicious
                    // evidence.
                    let bytes = fs::read(&path)
                        .map_err(|e| io_err("reclaim quarantined read", &path, e))?;
                    if hash_object(&bytes) == *identity {
                        fs::remove_file(&path)
                            .map_err(|e| io_err("reclaim quarantined", &path, e))?;
                        removed += 1;
                        let sidecar = path.with_file_name(format!("{name}.reason"));
                        if sidecar.is_file() {
                            fs::remove_file(&sidecar)
                                .map_err(|e| io_err("reclaim sidecar", &sidecar, e))?;
                            removed += 1;
                        }
                    }
                }
            }
        }
        partition.removed = removed;
        Ok(partition)
    }
}

/// Test-only (the genuine mid-unit reclamation crash seam, the
/// review corrective F4's real-kill evidence): when armed, the
/// reclamation delete hard-exits AFTER a unit's record object is
/// removed and BEFORE its referenced content objects — the exact
/// orphaning window the audited manifest closes on the retry (exit
/// 10, distinct from the after-SK seam's 8). Engine code never arms
/// this; the static is process-local, so only the child that armed
/// it dies.
#[doc(hidden)]
pub static ARM_RECLAIM_MID_UNIT: AtomicBool = AtomicBool::new(false);

/// The mid-unit seam's exit point (see [`ARM_RECLAIM_MID_UNIT`]).
fn reclaim_mid_unit_seam() {
    if ARM_RECLAIM_MID_UNIT.load(Ordering::Relaxed) {
        std::process::exit(10);
    }
}

/// Test-only (the genuine last-unit reclamation crash seam, the
/// review corrective R1's real-kill evidence): when armed, the
/// reclamation delete hard-exits AFTER the LAST unit's record object
/// is removed and BEFORE its referenced content objects — every
/// indexed record is then absent while the manifest is NOT complete,
/// the exact misclassification window the whole-manifest completion
/// probe closes (exit 11, distinct from the mid-unit seam's 10 and
/// the after-audit seam's 9). Engine code never arms this; the
/// static is process-local, so only the child that armed it dies.
#[doc(hidden)]
pub static ARM_RECLAIM_LAST_UNIT: AtomicBool = AtomicBool::new(false);

/// The last-unit seam's exit point (see [`ARM_RECLAIM_LAST_UNIT`]).
fn reclaim_last_unit_seam() {
    if ARM_RECLAIM_LAST_UNIT.load(Ordering::Relaxed) {
        std::process::exit(11);
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

#[cfg(test)]
mod exclusion_tests {
    use super::*;
    use crate::checkpoint::{CheckpointMetadata, ReclaimedIndexEntry, SkipEntry};
    use crate::hash::hash_object;
    use crate::medium::AppendOutcome;
    use crate::medium::CanonicalMedium;
    use crate::reclaim::publish_reclamation_ledger;
    use crate::reclaim::{
        CompletionKind, ReclaimUnit, ReclamationComplete, ReclamationLedger,
        publish_reclamation_complete,
    };
    use crate::record::{AuthorityMode, RevisionRecord};

    fn temp_store(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "s7-p2f1-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn snapshot(medium: &mut LocalFileMedium) -> (Option<Hash>, Option<ChainSummary>) {
        // The observable ordinary state: the trusted head and the
        // validated summary (the cursor's authoritative view).
        let head = medium.head().unwrap();
        let summary = medium.validate().ok();
        (head, summary)
    }

    #[test]
    fn a_refused_exclusion_walk_leaves_the_medium_untouched() {
        // P2-F1's regression: a nonempty virtual-exclusion walk that
        // REFUSES (here: the excluded record has no covering MK skip
        // — the uncovered virtual absence fails closed exactly like
        // an uncovered physical absence) leaves the provider's
        // ordinary state exactly as it was — the trusted head, the
        // validated chain, and a subsequent append all behave as if
        // the simulated walk never ran. The walk is a pure &self
        // computation: it STRUCTURALLY cannot touch the cursor, the
        // counters, or any sweep state.
        let root = temp_store("refusal");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        // A founded chain: the founding record plus two regular
        // revisions (no MK — any exclusion is uncovered).
        let object = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2f1", 0)
            .with_content_refs(vec![object.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&object), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let second = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![object.hash()]);
        medium
            .append(&second, std::slice::from_ref(&object), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let third = RevisionRecord::regular(head, epoch, 2).with_content_refs(vec![object.hash()]);
        medium
            .append(&third, std::slice::from_ref(&object), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();

        let before = snapshot(&mut medium);
        let before_stats = medium.walk_stats();

        // The nonempty exclusion over the middle record: with no
        // covering skip table this fails closed (the uncovered
        // virtual absence) — the exact pre-deletion refusal shape.
        let excluded: BTreeSet<Hash> = [second.id()].into_iter().collect();
        let refused = medium.cold_walk_excluding(head, &excluded);
        assert!(
            refused.is_err(),
            "the uncovered virtual exclusion fails closed"
        );

        // The ordinary state is byte-for-byte as before: the head,
        // the validation, the walk counters, and a fresh append all
        // behave exactly as if the simulated walk never ran.
        let after = snapshot(&mut medium);
        assert_eq!(before, after, "the trusted state is untouched");
        assert_eq!(
            medium.walk_stats().cold_walk_records,
            before_stats.cold_walk_records,
            "the simulated walk never counted as a cold walk"
        );
        let head_now = medium.head().unwrap().unwrap();
        let epoch_now = medium.read_record(&head_now).unwrap().epoch;
        let fourth =
            RevisionRecord::regular(head_now, epoch_now, 3).with_content_refs(vec![object.hash()]);
        let outcome = medium
            .append(&fourth, std::slice::from_ref(&object), Some(head_now))
            .unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));

        // The reopen behaves identically (the on-disk state was
        // never touched by the simulated view).
        drop(medium);
        let mut reopened = LocalFileMedium::open(&root).unwrap();
        assert_eq!(reopened.head().unwrap().unwrap(), fourth.id());
        reopened.validate().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_marker_publication_admits_verifies_and_cas_binds() {
        // P2-F4B/F4C: the marker's publication requires the CANONICAL
        // AUDIT AUTHORITY — a real checkpoint on the chain, a real
        // audit of its retirement index (the SK/digest/seed bindings
        // and the exact unit membership), the v1-only batch-zero
        // form — and the exact-HEAD CAS. The fabricated-MK marker
        // (the review's counterexample) refuses.
        let root = temp_store("marker-authority");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let object = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        // The real-checkpoint chain: F(0) → P1(1) → P2(2) → SK(3) →
        // MK(4), the index covering {P2, SK} with the self-skip to
        // P1.
        let mut chain: Vec<Hash> = Vec::new();
        let mut epochs: Vec<u64> = Vec::new();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2a", 0)
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
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let p2_bytes = medium.read_record(&p2).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[p2_bytes]);
        let metadata = CheckpointMetadata::new(
            sk,
            p2,
            vec![SkipEntry::new(Hash::ZERO, p1)],
            vec![
                ReclaimedIndexEntry::new(None, p2, epochs[2]),
                ReclaimedIndexEntry::new(None, sk, epochs[3]),
            ],
            range_digest,
            seed,
        );
        let meta_object = StoredObject::new(metadata.canonical_bytes()).unwrap();
        let mk_record =
            RevisionRecord::regular(sk, epochs[3], 5).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk_record, std::slice::from_ref(&meta_object), Some(sk))
            .unwrap();

        // A REAL audit of the checkpoint's retirement index (the
        // ledger's units ARE the index, NEWEST-FIRST; the bindings
        // ARE the metadata's).
        let p2_refs = medium.read_record(&p2).unwrap().content_refs.clone();
        let sk_refs = medium.read_record(&sk).unwrap().content_refs.clone();
        let ledger = ReclamationLedger {
            mk: mk_record.id(),
            sk,
            range_digest: metadata.range_digest,
            seed: metadata.seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                ReclaimUnit {
                    record: sk,
                    objects: sk_refs,
                },
                ReclaimUnit {
                    record: p2,
                    objects: p2_refs,
                },
            ],
        };
        let (audit_record, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 6).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        // The retirement EXECUTED — the marker's admission verifies
        // the actual post-execution state.
        let directive = crate::reclaim::ReclaimDirective {
            audit_record,
            units: ledger.units.clone(),
        };
        let execution = medium.reclaim(&directive).unwrap();
        let current = medium.head().unwrap().unwrap();

        // The FABRICATED-MK marker (the review's counterexample:
        // an audit-shaped source over a nonexistent checkpoint)
        // refuses — no checkpoint on the chain matches.
        let fabricated = ReclamationComplete {
            kind: CompletionKind::Batch,
            mk: hash_object(b"p2a-fabricated-mk"),
            batch: 0,
            source: audit_record,
            mandatory_count: 0,
            deferred: Vec::new(),
            journal_files: Vec::new(),
        };
        assert!(
            publish_reclamation_complete(&mut medium, &fabricated, current, 7).is_err(),
            "a marker over a fabricated MK refuses"
        );
        // (A1) The UNRELATED-SOURCE marker: a valid audit for the MK
        // exists, but the marker's source names a different record —
        // the exact-identity check refuses even though the real
        // audit is installed.
        let unrelated = ReclamationComplete {
            kind: CompletionKind::Batch,
            mk: mk_record.id(),
            batch: 0,
            source: hash_object(b"p2a-unrelated-source"),
            mandatory_count: 0,
            deferred: Vec::new(),
            journal_files: Vec::new(),
        };
        assert!(
            publish_reclamation_complete(&mut medium, &unrelated, current, 7).is_err(),
            "a marker whose source is not the installed audit refuses"
        );
        // A NONZERO batch index refuses (the v1 single-batch form).
        let nonzero = ReclamationComplete {
            kind: CompletionKind::Batch,
            mk: mk_record.id(),
            batch: 3,
            source: audit_record,
            mandatory_count: 0,
            deferred: Vec::new(),
            journal_files: Vec::new(),
        };
        assert!(
            publish_reclamation_complete(&mut medium, &nonzero, current, 7).is_err(),
            "a nonzero batch index refuses under the v1 protocol"
        );
        let now = current;
        // A marker whose units DISAGREE with the checkpoint index
        // is structurally unreachable through legitimate
        // publication: the deterministic one-key-per-MK uniqueness
        // refuses a second audit for the same checkpoint at
        // installation (verified above by the duplicate-key
        // refusal), and an off-chain hand-crafted audit record is
        // refused by the chain-membership check. The units-vs-index
        // cross-check remains the defense-in-depth for the
        // compromised-key scenario, exercised at the verification
        // layer in the execution-switch increment.
        //
        // P2-F4: the RECEIPT check — a caller-constructed marker
        // whose certified count disagrees with the verified state
        // refuses (the admission never trusts the claim).
        let wrong_count = ReclamationComplete {
            kind: CompletionKind::Batch,
            mk: mk_record.id(),
            batch: 0,
            source: audit_record,
            mandatory_count: execution.mandatory.len() as u64 + 1,
            deferred: execution.deferred.clone(),
            journal_files: Vec::new(),
        };
        let error = publish_reclamation_complete(&mut medium, &wrong_count, now, 9).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/mandatory-count-mismatch"
                }
            ),
            "the mismatched receipt refuses: {error:?}"
        );
        // The ADMITTED marker over the REAL executed retirement:
        // constructed from the verified execution outcome,
        // CAS-bound (a HEAD advancement refuses; the current head
        // accepts).
        let marker = ReclamationComplete {
            kind: CompletionKind::Batch,
            mk: mk_record.id(),
            batch: 0,
            source: audit_record,
            mandatory_count: execution.mandatory.len() as u64,
            deferred: execution.deferred.clone(),
            journal_files: Vec::new(),
        };
        let epoch = medium.read_record(&now).unwrap().epoch;
        let interloper =
            RevisionRecord::regular(now, epoch, 10).with_content_refs(vec![object.hash()]);
        medium
            .append(&interloper, std::slice::from_ref(&object), Some(now))
            .unwrap();
        let (_, outcome) = publish_reclamation_complete(&mut medium, &marker, now, 11).unwrap();
        assert!(
            matches!(outcome, AppendOutcome::RejectedStaleParent { .. }),
            "the advanced HEAD refuses the stale-parent marker"
        );
        let latest = medium.head().unwrap().unwrap();
        let (_, outcome) = publish_reclamation_complete(&mut medium, &marker, latest, 12).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        medium.validate().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_verifier_crosses_a_reclaimed_gap_to_reach_the_target() {
        // (B2) The GENUINE gap crossing: the target audit and MK sit
        // BELOW a later checkpoint whose retirement range has been
        // physically reclaimed — the descent must cross the
        // authenticated missing-parent gap BEFORE reaching the
        // target pair.
        let root = temp_store("gap-crossing");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let object = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        // Each append reads the CURRENT head's epoch from the medium
        // itself (no side-tracked arrays).
        let step = |medium: &mut LocalFileMedium, tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record =
                RevisionRecord::regular(head, epoch, tick).with_content_refs(vec![object.hash()]);
            medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap();
            record.id()
        };
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2b2", 0)
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
        // The target audit (the correct manifest: R1's actual refs).
        let ledger = ReclamationLedger {
            mk: mk_o.id(),
            sk: r1,
            range_digest: old_meta.range_digest,
            seed: old_meta.seed,
            records: 1,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![ReclaimUnit {
                record: r1,
                objects: vec![object.hash()],
            }],
        };
        let (audit_o, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 10).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        // The NEW range above the audit (to be reclaimed).
        let p2n = step(&mut medium, 11);
        let p2n_epoch = medium.read_record(&p2n).unwrap().epoch;
        let sk_n = step(&mut medium, 12);
        let sk_n_epoch = medium.read_record(&sk_n).unwrap().epoch;
        let new_meta = CheckpointMetadata::new(
            sk_n,
            p2n,
            // The self-skip targets the NEAREST RETAINED ancestor
            // below the reclaimed span {P2n, SK_n} — the audit
            // revision itself (the retained record standing on the
            // reclaimed range's top).
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

        // THE RECLAIMED GAP: the NEW range is physically deleted.
        std::fs::remove_file(medium.object_path(&p2n)).unwrap();
        std::fs::remove_file(medium.object_path(&sk_n)).unwrap();

        // (B2) The target pair — BELOW the gap — verifies: the
        // descent from MK_new meets the absent SK_n, crosses via
        // MK_new's self-skip to audit_o (the NEAREST RETAINED
        // ancestor — the retained record on the reclaimed span's
        // top), and
        // authenticates the exact audit and MK. The crossing was
        // NECESSARY (the gap is physical).
        let (verified, metadata) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit_o, &mk_o.id()).unwrap();
        assert_eq!(verified.mk, mk_o.id());
        assert_eq!(metadata.sk, r1);
        assert_eq!(verified.units.len(), 1);
        assert!(!medium.object_path(&sk_n).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
    /// The shared first-installed wrong-manifest fixture: a valid
    /// checkpoint, a genuine range record, and a FIRST audit
    /// installed through the ordinary publication route — correct
    /// identities, bindings, and key, but a deliberately incorrect
    /// object manifest.
    fn wrong_manifest_store(
        wrong_objects: Vec<Hash>,
    ) -> (std::path::PathBuf, LocalFileMedium, Hash, Hash) {
        let root = temp_store("wrong-manifest");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let object = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2c3", 0)
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
        let mk = RevisionRecord::regular(r1.id(), 1, 2).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(r1.id()))
            .unwrap();
        let ledger = ReclamationLedger {
            mk: mk.id(),
            sk: r1.id(),
            range_digest: meta.range_digest,
            seed: meta.seed,
            records: 1,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![ReclaimUnit {
                record: r1.id(),
                objects: wrong_objects,
            }],
        };
        let (audit, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 3).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        (root, medium, audit, mk.id())
    }

    #[test]
    fn the_first_installed_audit_omitting_a_referenced_object_refuses() {
        // (B3, omission) A FIRST installed audit — the valid
        // checkpoint, the correct record identity, bindings, and
        // key — whose unit OMITS the object the retiring record
        // actually references. The audit IS on the authoritative
        // chain; the verifier reaches the manifest comparison and
        // refuses with the exact class; nothing was deleted.
        let (root, mut medium, audit, mk) = wrong_manifest_store(Vec::new());
        let error = crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk)
            .expect_err("the omitted manifest refuses at the comparison");
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/units-objects-mismatch"
                }
            ),
            "the exact error class: {error:?}"
        );
        assert!(medium.object_path(&audit).is_file(), "nothing was deleted");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_first_installed_audit_injecting_an_unrelated_object_refuses() {
        // (B3, injection) The same first-installed shape, but the
        // unit includes an UNRELATED object identity the record
        // never referenced.
        let (root, mut medium, audit, mk) =
            wrong_manifest_store(vec![hash_object(b"p2c3-injected")]);
        let error = crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk)
            .expect_err("the injected manifest refuses at the comparison");
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/units-objects-mismatch"
                }
            ),
            "the exact error class: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_survivor_partition_defers_shared_and_deduplicates() {
        // The trusted directive's decision core: over the
        // real-checkpoint fixture, the partition (a) DEFERS an
        // object a surviving record also references (never deleted
        // merely because a retiring record names it), (b) classifies
        // a multi-unit shared object exactly ONCE, and (c) marks
        // the retiring records and the unshared objects MANDATORY.
        let root = temp_store("partition");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let shared = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"shared-payload");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"private-payload");
            enc.into_bytes()
        })
        .unwrap();
        // F(0) → P1(1, references SHARED — the surviving root) →
        // R1(2, SHARED + PRIVATE) → R2(3, SHARED) → SK(4) → MK(5,
        // the index {R1, R2, SK}, the self-skip to P1).
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2p", 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let p1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared.hash()]);
        medium
            .append(&p1, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let r1 = RevisionRecord::regular(head, epoch, 2)
            .with_content_refs(vec![shared.hash(), private.hash()]);
        medium
            .append(&r1, &[shared.clone(), private.clone()], Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let r2 = RevisionRecord::regular(head, epoch, 3).with_content_refs(vec![shared.hash()]);
        medium
            .append(&r2, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sk = RevisionRecord::regular(head, epoch, 4).with_content_refs(vec![shared.hash()]);
        medium
            .append(&sk, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let meta = CheckpointMetadata::new(
            sk.id(),
            r2.id(),
            vec![SkipEntry::new(Hash::ZERO, p1.id())],
            vec![
                ReclaimedIndexEntry::new(None, r1.id(), 2),
                ReclaimedIndexEntry::new(None, r2.id(), 3),
                ReclaimedIndexEntry::new(None, sk.id(), 4),
            ],
            [1; 32],
            [2; 32],
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let mk =
            RevisionRecord::regular(sk.id(), epoch, 5).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(sk.id()))
            .unwrap();
        let head = mk.id();

        // The survivor set: the exclusion walk over the audited
        // range {R1, R2, SK}.
        let excluded: BTreeSet<Hash> = [r1.id(), r2.id(), sk.id()].into_iter().collect();
        let (_summary, simulated) = medium.cold_walk_excluding(head, &excluded).unwrap();
        // The audited units (the correct manifests): R1 carries
        // SHARED + PRIVATE; R2 carries SHARED; SK carries SHARED —
        // SHARED appears in THREE units.
        let units = vec![
            ReclaimUnit {
                record: r1.id(),
                objects: vec![shared.hash(), private.hash()],
            },
            ReclaimUnit {
                record: r2.id(),
                objects: vec![shared.hash()],
            },
            ReclaimUnit {
                record: sk.id(),
                objects: vec![shared.hash()],
            },
        ];
        let outcome = crate::reclaim::partition_reclaim(&units, &simulated.reachable).unwrap();
        // SHARED is DEFERRED (P1 — a surviving root — references it),
        // classified exactly once despite three unit appearances.
        assert_eq!(outcome.deferred, vec![shared.hash()]);
        // MANDATORY: the three retiring records + PRIVATE (referenced
        // only by the retiring R1) — SHARED absent.
        let mut expected_mandatory = vec![r1.id(), private.hash(), r2.id(), sk.id()];
        expected_mandatory.sort();
        let mut mandatory = outcome.mandatory.clone();
        mandatory.sort();
        assert_eq!(mandatory, expected_mandatory);
        assert!(!mandatory.contains(&shared.hash()));
        let _ = std::fs::remove_dir_all(&root);
    }
    /// The shared trusted-execution fixture: the real-checkpoint
    /// chain with a surviving root sharing an object with the
    /// retiring range, and the installed audit.
    fn trusted_execution_store() -> (std::path::PathBuf, LocalFileMedium, Hash, Hash, Hash, Hash) {
        let root = temp_store("trusted-exec");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let shared = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"shared-payload");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"private-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2tx", 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let p1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared.hash()]);
        medium
            .append(&p1, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let r1 = RevisionRecord::regular(head, epoch, 2)
            .with_content_refs(vec![shared.hash(), private.hash()]);
        medium
            .append(&r1, &[shared.clone(), private.clone()], Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sk = RevisionRecord::regular(head, epoch, 3).with_content_refs(vec![shared.hash()]);
        medium
            .append(&sk, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        // The E1 authentic commitments: the seed derives from the
        // campaign founding; the range digest is COMPUTED over the
        // retiring publications' canonical bytes (the Phase A
        // discipline). The E2 order: the units run NEWEST-first
        // (SK, then R1 — descending epoch).
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let range_digest = crate::checkpoint::chain_range_digest(&seed, &[r1.canonical_bytes()]);
        let meta = CheckpointMetadata::new(
            sk.id(),
            r1.id(),
            vec![SkipEntry::new(Hash::ZERO, p1.id())],
            vec![
                ReclaimedIndexEntry::new(None, r1.id(), 2),
                ReclaimedIndexEntry::new(None, sk.id(), 3),
            ],
            range_digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let mk =
            RevisionRecord::regular(sk.id(), epoch, 4).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(sk.id()))
            .unwrap();
        // The units' object lists use the records' CANONICAL
        // reference order (with_content_refs sorts — the manifest
        // must match the canonical form exactly).
        let r1_refs = medium.read_record(&r1.id()).unwrap().content_refs.clone();
        let sk_refs = medium.read_record(&sk.id()).unwrap().content_refs.clone();
        let ledger = ReclamationLedger {
            mk: mk.id(),
            sk: sk.id(),
            range_digest: meta.range_digest,
            seed: meta.seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                ReclaimUnit {
                    record: sk.id(),
                    objects: sk_refs,
                },
                ReclaimUnit {
                    record: r1.id(),
                    objects: r1_refs,
                },
            ],
        };
        let (audit, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        (root, medium, audit, mk.id(), shared.hash(), private.hash())
    }

    #[test]
    fn the_trusted_directive_executes_with_shared_object_survival() {
        // The successful authenticated directive: the retiring
        // records and the PRIVATE object deleted; the SHARED object
        // (referenced by the surviving P1) SURVIVES; the deferred
        // classification reported; the post-retirement validation,
        // append, and reopen all work; the pending probe reports
        // complete.
        let (root, mut medium, audit, mk, shared, private) = trusted_execution_store();
        // The units come from the ledger (the verifier's own decode
        // — the caller's candidate must match the installed
        // manifest exactly).
        let (ledger, _meta) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        let outcome = medium.reclaim(&directive).unwrap();
        // The deferred set: the shared object (never deleted).
        assert_eq!(outcome.deferred, vec![shared]);
        assert!(!outcome.mandatory.contains(&shared));
        // The private object and the two retiring records are gone.
        assert!(!medium.object_path(&private).is_file());
        // The shared object survives on disk.
        assert!(medium.object_path(&shared).is_file());
        // The pending probe: complete (no mandatory form remains).
        assert!(!medium.reclaim_pending(&directive).unwrap());
        // The post-retirement chain validates; a fresh append works;
        // the reopen behaves.
        medium.validate().unwrap();
        let append_object = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"post-retirement");
            enc.into_bytes()
        })
        .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let after =
            RevisionRecord::regular(head, epoch, 9).with_content_refs(vec![append_object.hash()]);
        let outcome = medium
            .append(&after, std::slice::from_ref(&append_object), Some(head))
            .unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        drop(medium);
        let mut reopened = LocalFileMedium::open(&root).unwrap();
        reopened.validate().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_wrong_units_directive_refuses_leaving_storage_unchanged() {
        // The candidate manifest that disagrees with the installed
        // ledger refuses BEFORE any mutation — the physical storage
        // unchanged.
        let (root, mut medium, audit, mk, _shared, private) = trusted_execution_store();
        let (ledger, _meta) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
        let mut skewed = ledger.units.clone();
        skewed[0].objects = Vec::new();
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: skewed,
        };
        let error = medium.reclaim(&directive).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclaim/directive-units-mismatch"
                }
            ),
            "the exact refusal: {error:?}"
        );
        // Nothing was deleted: the private object and both records
        // remain.
        assert!(medium.object_path(&private).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_interrupted_execution_resumes_from_the_persisted_audit() {
        // The interrupted recovery: the record pass partially
        // executed (one record physically deleted by hand — the
        // crash shape); the SAME directive resumes and completes
        // (the absent record's manifest reread is skipped — the
        // durable audit accounts for it), the shared object still
        // survives, and the result converges.
        let (root, mut medium, audit, mk, shared, private) = trusted_execution_store();
        let (ledger, _meta) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
        // The crash shape: the FIRST unit's record is gone; its
        // objects remain.
        let interrupted_record = ledger.units[0].record;
        std::fs::remove_file(medium.object_path(&interrupted_record)).unwrap();
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        let outcome = medium.reclaim(&directive).unwrap();
        assert_eq!(outcome.deferred, vec![shared]);
        assert!(!medium.object_path(&private).is_file());
        assert!(medium.object_path(&shared).is_file());
        assert!(!medium.reclaim_pending(&directive).unwrap());
        medium.validate().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_fabricated_range_commitments_and_wrong_order_refuse() {
        // (E1) The equal-but-FABRICATED stored commitments: the
        // metadata and the ledger carry the SAME wrong digest (the
        // seed correctly founding-derived) — the provider RECOMPUTES
        // the range digest over the canonical bytes and refuses
        // before any deletion. (E2) The oldest-first unit order
        // refuses. (E4) The post-retirement append works WITHOUT a
        // manual validate (the cursor invalidation rebuilds).
        // A parameterized fixture: the digest commitment and the
        // unit order are the two knobs (the on-chain records and the
        // metadata are immutable, so the fabricated commitments live
        // in a store built with them from the start).
        let build = |label: &str, digest_override: Option<[u8; 32]>, order_oldest_first: bool| {
            let root = temp_store(label);
            let mut medium = LocalFileMedium::open(root.clone()).unwrap();
            let shared = StoredObject::new({
                let mut enc = engine_core::Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"shared-payload");
                enc.into_bytes()
            })
            .unwrap();
            let private = StoredObject::new({
                let mut enc = engine_core::Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"private-payload");
                enc.into_bytes()
            })
            .unwrap();
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2e", 0)
                .with_content_refs(vec![shared.hash()]);
            medium
                .append(&founding, std::slice::from_ref(&shared), None)
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let p1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared.hash()]);
            medium
                .append(&p1, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let r1 = RevisionRecord::regular(head, epoch, 2)
                .with_content_refs(vec![shared.hash(), private.hash()]);
            medium
                .append(&r1, &[shared.clone(), private.clone()], Some(head))
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let sk = RevisionRecord::regular(head, epoch, 3).with_content_refs(vec![shared.hash()]);
            medium
                .append(&sk, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let seed = crate::checkpoint::compute_digest_seed(&founding.id());
            let real_digest = crate::checkpoint::chain_range_digest(&seed, &[r1.canonical_bytes()]);
            let digest = digest_override.unwrap_or(real_digest);
            let meta = CheckpointMetadata::new(
                sk.id(),
                r1.id(),
                vec![SkipEntry::new(Hash::ZERO, p1.id())],
                vec![
                    ReclaimedIndexEntry::new(None, r1.id(), 2),
                    ReclaimedIndexEntry::new(None, sk.id(), 3),
                ],
                digest,
                seed,
            );
            let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
            let mk = RevisionRecord::regular(sk.id(), epoch, 4)
                .with_content_refs(vec![meta_object.hash()]);
            medium
                .append(&mk, std::slice::from_ref(&meta_object), Some(sk.id()))
                .unwrap();
            let r1_refs = medium.read_record(&r1.id()).unwrap().content_refs.clone();
            let sk_refs = medium.read_record(&sk.id()).unwrap().content_refs.clone();
            let mut units = vec![
                ReclaimUnit {
                    record: sk.id(),
                    objects: sk_refs,
                },
                ReclaimUnit {
                    record: r1.id(),
                    objects: r1_refs,
                },
            ];
            if order_oldest_first {
                units.reverse();
            }
            let ledger = ReclamationLedger {
                mk: mk.id(),
                sk: sk.id(),
                range_digest: digest,
                seed,
                records: 2,
                reclaimed_keys: 0,
                journal_files: Vec::new(),
                units,
            };
            let (audit, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
            assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
            (
                root,
                medium,
                audit,
                mk.id(),
                shared.hash(),
                private.hash(),
                ledger,
            )
        };

        // (E1) The fabricated digest (equal in the metadata and the
        // ledger, the seed correct): the recomputation refuses;
        // nothing deleted.
        {
            let (root, mut medium, audit, mk, _shared, private, _ledger) =
                build("e1-fabricated", Some([9; 32]), false);
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            let error = medium.reclaim(&directive).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclaim/range-digest-mismatch"
                    }
                ),
                "the fabricated digest refuses: {error:?}"
            );
            assert!(
                medium.object_path(&private).is_file(),
                "nothing was deleted"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (E2) The oldest-first unit order: the order check refuses.
        {
            let (root, mut medium, audit, mk, _shared, private, _ledger) =
                build("e2-oldest-first", None, true);
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            let error = medium.reclaim(&directive).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclaim/unit-order-not-newest-first"
                    }
                ),
                "the oldest-first order refuses: {error:?}"
            );
            assert!(
                medium.object_path(&private).is_file(),
                "nothing was deleted"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (E4) The legitimate execution + the append WITHOUT a
        // manual validate: the cursor invalidation rebuilds.
        {
            let (root, mut medium, audit, mk, _shared, _private, _ledger) =
                build("e4-legitimate", None, false);
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            medium.reclaim(&directive).unwrap();
            // NO validate() call — the immediate append must use the
            // rebuilt post-retirement state.
            let object = StoredObject::new({
                let mut enc = engine_core::Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"post-retirement-no-validate");
                enc.into_bytes()
            })
            .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record =
                RevisionRecord::regular(head, epoch, 10).with_content_refs(vec![object.hash()]);
            let outcome = medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap();
            assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn a_fabricated_audit_with_a_non_prefix_absence_refuses() {
        // (E1) The recovery-authorization boundary: a missing
        // retiring publication in a NON-prefix pattern (the oldest
        // digest member gone while the SK — the newest unit —
        // remains) is a shape pass 1 never produces. Physical
        // absence is evidence of missing storage, never proof of a
        // prior authorized deletion: the FABRICATED audit cannot
        // ride the resume tolerance, and a genuine audit that lost a
        // non-prefix record fails closed the same way.
        let build = |label: &str, digest_override: Option<[u8; 32]>| {
            let root = temp_store(label);
            let mut medium = LocalFileMedium::open(root.clone()).unwrap();
            let shared = StoredObject::new({
                let mut enc = engine_core::Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"shared-payload");
                enc.into_bytes()
            })
            .unwrap();
            let private = StoredObject::new({
                let mut enc = engine_core::Encoder::new();
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
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let p1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared.hash()]);
            medium
                .append(&p1, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let r1 = RevisionRecord::regular(head, epoch, 2)
                .with_content_refs(vec![shared.hash(), private.hash()]);
            medium
                .append(&r1, &[shared.clone(), private.clone()], Some(head))
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let sk = RevisionRecord::regular(head, epoch, 3).with_content_refs(vec![shared.hash()]);
            medium
                .append(&sk, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let seed = crate::checkpoint::compute_digest_seed(&founding.id());
            let real_digest = crate::checkpoint::chain_range_digest(&seed, &[r1.canonical_bytes()]);
            let digest = digest_override.unwrap_or(real_digest);
            let meta = CheckpointMetadata::new(
                sk.id(),
                r1.id(),
                vec![SkipEntry::new(Hash::ZERO, p1.id())],
                vec![
                    ReclaimedIndexEntry::new(None, r1.id(), 2),
                    ReclaimedIndexEntry::new(None, sk.id(), 3),
                ],
                digest,
                seed,
            );
            let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
            let mk = RevisionRecord::regular(sk.id(), epoch, 4)
                .with_content_refs(vec![meta_object.hash()]);
            medium
                .append(&mk, std::slice::from_ref(&meta_object), Some(sk.id()))
                .unwrap();
            let r1_refs = medium.read_record(&r1.id()).unwrap().content_refs.clone();
            let sk_refs = medium.read_record(&sk.id()).unwrap().content_refs.clone();
            let ledger = ReclamationLedger {
                mk: mk.id(),
                sk: sk.id(),
                range_digest: digest,
                seed,
                records: 2,
                reclaimed_keys: 0,
                journal_files: Vec::new(),
                units: vec![
                    ReclaimUnit {
                        record: sk.id(),
                        objects: sk_refs,
                    },
                    ReclaimUnit {
                        record: r1.id(),
                        objects: r1_refs,
                    },
                ],
            };
            let (audit, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
            assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
            (root, medium, audit, shared.hash(), private.hash(), ledger)
        };
        // The fabricated digest + the non-prefix absence: the
        // provider must not continue deletion merely because the
        // missing record resembles an interrupted retirement.
        {
            let (root, mut medium, audit, shared, private, ledger) =
                build("e1-np-fabricated", Some([9; 32]));
            let oldest = ledger.units.last().unwrap().record;
            std::fs::remove_file(medium.object_path(&oldest)).unwrap();
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
            // Nothing further deleted: the SK record and every
            // object remain.
            let newest = ledger.units.first().unwrap().record;
            assert!(medium.object_path(&newest).is_file());
            assert!(medium.object_path(&private).is_file());
            assert!(medium.object_path(&shared).is_file());
            let _ = std::fs::remove_dir_all(&root);
        }
        // The GENUINE digest + the non-prefix absence (external
        // storage loss): the same fail-closed refusal — the absence
        // pattern is the evidence, independent of the audit's
        // authenticity.
        {
            let (root, mut medium, audit, _shared, _private, ledger) = build("e1-np-genuine", None);
            let oldest = ledger.units.last().unwrap().record;
            std::fs::remove_file(medium.object_path(&oldest)).unwrap();
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
                "the genuine audit fails closed on loss: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }
    #[test]
    fn a_probe_read_failure_is_typed_not_silent() {
        // (E5) A read failure on a mandatory identity's normal path
        // is NOT an absence: the probe refuses typed — an
        // unverifiable observation never silently contributes to a
        // completed result. A directory at the object path is the
        // deterministic cross-platform unreadable read.
        let (root, mut medium, audit, mk, _shared, private) = trusted_execution_store();
        let (ledger, _meta) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        // The retirement executed: every mandatory identity is gone,
        // the probe reports complete — the completion-critical
        // observation.
        medium.reclaim(&directive).unwrap();
        assert!(!medium.reclaim_pending(&directive).unwrap());
        // A directory at the (now absent) object path is the
        // deterministic cross-platform unreadable read: the probe
        // must refuse typed, never silently report complete over an
        // unverifiable observation.
        let private_path = medium.object_path(&private);
        std::fs::create_dir(&private_path).unwrap();
        let error = medium.reclaim_pending(&directive).unwrap_err();
        assert!(
            matches!(error, MediumError::Io { .. }),
            "the read failure is a typed refusal: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_trusted_completion_receipt_installs_verifies_and_resumes() {
        // (P2-F4) The trusted completion receipt: after the
        // executed retirement, complete_reclamation verifies the
        // recovery-safe audit authority and the ACTUAL
        // postconditions, constructs the marker FROM the verified
        // state, and installs it through the CAS-bound admission.
        // The trusted consumer verifies the installed marker; a
        // fresh-process reopen rediscovers it; a repeated call
        // resolves the duplicate deterministic key by DISCOVERING
        // and exactly verifying the installed marker — never
        // inferring success from the key alone.
        let (root, mut medium, audit, mk, shared, private) = trusted_execution_store();
        let (ledger, _meta) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        medium.reclaim(&directive).unwrap();
        // The trusted receipt path.
        let (marker_id, marker) =
            crate::reclaim::complete_reclamation(&mut medium, &audit, &mk).unwrap();
        assert_eq!(marker.kind, crate::reclaim::CompletionKind::Batch);
        assert_eq!(marker.batch, 0);
        assert_eq!(marker.source, audit);
        assert_eq!(marker.deferred, vec![shared]);
        // The installed marker discovered and verified — the
        // read-side authority.
        let (found_id, found) = crate::reclaim::find_installed_completion(&mut medium, &mk)
            .unwrap()
            .expect("the marker is discoverable");
        assert_eq!(found_id, marker_id);
        assert_eq!(found, marker);
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk).unwrap();
        // The mandatory-empty postcondition holds; the deferred
        // survivor is present; the private object is gone.
        assert!(!medium.reclaim_pending(&directive).unwrap());
        assert!(medium.object_path(&shared).is_file());
        assert!(!medium.object_path(&private).is_file());
        // The duplicate deterministic key: the repeated call
        // discovers and exactly verifies the installed marker.
        let (repeat_id, repeat) =
            crate::reclaim::complete_reclamation(&mut medium, &audit, &mk).unwrap();
        assert_eq!(repeat_id, marker_id);
        assert_eq!(repeat, marker);
        // The fresh-process reopen: the discovery and the
        // verification survive the restart.
        drop(medium);
        let mut reopened = LocalFileMedium::open(&root).unwrap();
        let (reopen_id, reopen_marker) =
            crate::reclaim::find_installed_completion(&mut reopened, &mk)
                .unwrap()
                .expect("the marker survives the reopen");
        assert_eq!(reopen_id, marker_id);
        assert_eq!(reopen_marker, marker);
        crate::reclaim::verify_reclamation_complete(&mut reopened, &marker_id, &mk).unwrap();
        reopened.validate().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn an_incomplete_retirement_refuses_completion() {
        // (P2-F4) The admission's state verification: a retirement
        // whose records are still present (the deletion never ran)
        // cannot certify completion — the typed refusal names the
        // still-present record, and nothing is installed.
        let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
        let error = crate::reclaim::complete_reclamation(&mut medium, &audit, &mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/record-still-present"
                }
            ),
            "the incomplete retirement refuses: {error:?}"
        );
        assert!(
            crate::reclaim::find_installed_completion(&mut medium, &mk)
                .unwrap()
                .is_none(),
            "no marker was installed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_completion_consumer_independently_verifies_terminal_authority() {
        // (P2-R2) The reader's verification is INDEPENDENT of the
        // writer's honesty: a hash-valid, keyed, correctly-sourced
        // marker that is NOT on the authoritative ancestry refuses;
        // an ON-CHAIN marker whose certified count or deferred set
        // disagrees with the recomputed obligations refuses; a
        // marker over an unexecuted retirement refuses. The
        // adversarial on-chain installs use the in-crate admitted
        // append directly — exactly the bypass a corrupted in-crate
        // path would represent.
        let install =
            |medium: &mut LocalFileMedium, marker: &crate::reclaim::ReclamationComplete| -> Hash {
                let head = medium.head().unwrap().unwrap();
                let parent_epoch = medium.read_record(&head).unwrap().epoch;
                let key = crate::reclaim::reclamation_complete_idempotency_key(
                    &marker.mk,
                    Some(marker.batch),
                );
                let object = StoredObject::new(marker.canonical_bytes()).unwrap();
                let record = RevisionRecord::regular(head, parent_epoch, 99)
                    .with_idempotency_key(key)
                    .with_content_refs(vec![object.hash()]);
                let admitted =
                    crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
                let outcome = medium.append_admitted(admitted, Some(head)).unwrap();
                assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
                record.id()
            };
        // (1) The wrong-count marker ON the chain: the consumer's
        // strict newest-evidence recheck refuses.
        {
            let (root, mut medium, audit, mk, shared, private) = trusted_execution_store();
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            medium.reclaim(&directive).unwrap();
            let wrong = crate::reclaim::ReclamationComplete {
                kind: crate::reclaim::CompletionKind::Batch,
                mk,
                batch: 0,
                source: audit,
                mandatory_count: 9,
                deferred: vec![shared],
                journal_files: Vec::new(),
            };
            let marker_id = install(&mut medium, &wrong);
            let error = crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk)
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-complete/mandatory-count-mismatch"
                    }
                ),
                "the wrong count refuses: {error:?}"
            );
            let _ = (private, shared);
            let _ = std::fs::remove_dir_all(&root);
        }
        // (2) The wrong-deferred marker ON the chain.
        {
            let (root, mut medium, audit, mk, _shared, private) = trusted_execution_store();
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            medium.reclaim(&directive).unwrap();
            let wrong = crate::reclaim::ReclamationComplete {
                kind: crate::reclaim::CompletionKind::Batch,
                mk,
                batch: 0,
                source: audit,
                mandatory_count: 3,
                deferred: vec![private],
                journal_files: Vec::new(),
            };
            let marker_id = install(&mut medium, &wrong);
            let error = crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk)
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-complete/deferred-mismatch"
                    }
                ),
                "the wrong deferred set refuses: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (3) The marker over an UNEXECUTED retirement: the
        // record-still-present refusal.
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let marker = crate::reclaim::ReclamationComplete {
                kind: crate::reclaim::CompletionKind::Batch,
                mk,
                batch: 0,
                source: audit,
                mandatory_count: 3,
                deferred: Vec::new(),
                journal_files: Vec::new(),
            };
            let marker_id = install(&mut medium, &marker);
            let error = crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk)
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-complete/record-still-present"
                    }
                ),
                "the uncertified completion refuses: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (4) The OFF-CHAIN marker: hash-valid, keyed, correctly
        // sourced — but hand-written into the object namespace, never
        // appended: the membership descent never encounters it.
        {
            let (root, mut medium, audit, mk, shared, _private) = trusted_execution_store();
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            medium.reclaim(&directive).unwrap();
            let (marker_id, marker) =
                crate::reclaim::complete_reclamation(&mut medium, &audit, &mk).unwrap();
            // The off-chain variant: a DIFFERENT record (a different
            // parent/tick) carrying the same marker object — written
            // directly into the object namespace, never appended.
            let head = medium.head().unwrap().unwrap();
            let parent_epoch = medium.read_record(&head).unwrap().epoch;
            let object = StoredObject::new(marker.canonical_bytes()).unwrap();
            let off_chain = RevisionRecord::regular(head, parent_epoch, 123)
                .with_idempotency_key(crate::reclaim::reclamation_complete_idempotency_key(
                    &mk,
                    Some(0),
                ))
                .with_content_refs(vec![object.hash()]);
            std::fs::write(medium.object_path(&object.hash()), object.bytes()).unwrap();
            std::fs::write(
                medium.object_path(&off_chain.id()),
                off_chain.canonical_bytes(),
            )
            .unwrap();
            let error =
                crate::reclaim::verify_reclamation_complete(&mut medium, &off_chain.id(), &mk)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-complete/marker-not-installed"
                    }
                ),
                "the off-chain marker refuses: {error:?}"
            );
            // The legitimate installed marker still verifies.
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk).unwrap();
            let _ = shared;
            let _ = std::fs::remove_dir_all(&root);
        }
    }
    #[test]
    #[cfg(feature = "composite-test-fixture")]
    fn the_composite_survivor_closure_is_parity_exact() {
        // (P2-R3) The completion verification's survivor closure is
        // PARITY-EXACT with the provider's authenticated exclusion
        // walk: a surviving /2 snapshot root references a chunk
        // through its verified manifest; a retiring record directly
        // references the SAME chunk. The provider classifies it
        // Deferred (preserved); the trait-level completion walk must
        // agree — the marker records it deferred and the completion
        // does not refuse over the legitimately preserved chunk.
        crate::composite_append::test_enable_composite_publication();
        let root = temp_store("r3-composite");
        let mut medium = LocalFileMedium::open(root.clone()).unwrap();
        // The full-grammar manifest fixture (the known-valid section
        // structure); the LEDGER TAIL chunk is the shared dependency
        // the retiring record also references.
        let mut seed = 3u8;
        let mut entries = Vec::new();
        let mut chunks = Vec::new();
        for tag in [
            crate::composite_manifest::TAG_HEADER,
            crate::composite_manifest::TAG_NEAR,
            crate::composite_manifest::TAG_FAR,
            crate::composite_manifest::TAG_BLOCKED,
            crate::composite_manifest::TAG_HELD,
            crate::composite_manifest::TAG_LEDGER_META,
        ] {
            let payload = vec![seed; 10 + seed as usize];
            seed = seed.wrapping_add(1);
            let object = crate::composite_manifest::encode_chunk_envelope(tag, &payload);
            entries.push(crate::composite_manifest::ManifestEntry {
                tag,
                identity: object.hash(),
                payload_length: payload.len() as u64,
                kind: crate::composite_manifest::SectionKind::Singleton,
            });
            chunks.push(object);
        }
        let sealed_width = 4u64 * 31;
        for _ in 0..2 {
            let payload = vec![seed; sealed_width as usize];
            seed = seed.wrapping_add(1);
            let object = crate::composite_manifest::encode_chunk_envelope(
                crate::composite_manifest::TAG_LEDGER_RECORDS,
                &payload,
            );
            entries.push(crate::composite_manifest::ManifestEntry {
                tag: crate::composite_manifest::TAG_LEDGER_RECORDS,
                identity: object.hash(),
                payload_length: sealed_width,
                kind: crate::composite_manifest::SectionKind::LedgerSealed,
            });
            chunks.push(object);
        }
        let tail_payload = vec![seed; 2 * 31];
        let shared_chunk = crate::composite_manifest::encode_chunk_envelope(
            crate::composite_manifest::TAG_LEDGER_RECORDS,
            &tail_payload,
        );
        entries.push(crate::composite_manifest::ManifestEntry {
            tag: crate::composite_manifest::TAG_LEDGER_RECORDS,
            identity: shared_chunk.hash(),
            payload_length: tail_payload.len() as u64,
            kind: crate::composite_manifest::SectionKind::LedgerTail,
        });
        for tag in [
            crate::composite_manifest::TAG_WORLD,
            crate::composite_manifest::TAG_DIRECTOR,
        ] {
            let payload = vec![seed; 10 + seed as usize];
            seed = seed.wrapping_add(1);
            let object = crate::composite_manifest::encode_chunk_envelope(tag, &payload);
            entries.push(crate::composite_manifest::ManifestEntry {
                tag,
                identity: object.hash(),
                payload_length: payload.len() as u64,
                kind: crate::composite_manifest::SectionKind::Singleton,
            });
            chunks.push(object);
        }
        let manifest = crate::composite_manifest::CompositeManifest {
            history_kind: crate::composite_manifest::HistoryKind::Full,
            ledger_boundary: 4,
            composed_length: entries.iter().map(|entry| entry.payload_length).sum(),
            entries,
        };
        let snapshot_root =
            StoredObject::new(crate::composite_manifest::encode(&manifest)).unwrap();
        // The chain: F(shared) → R1(the shared chunk as its content
        // object) → SK(shared) → MK → SNAPSHOT (the root-only
        // composite record reusing the shared chunk).
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"plain-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2r3", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let r1 =
            RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared_chunk.hash()]);
        medium
            .append(&r1, std::slice::from_ref(&shared_chunk), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sk = RevisionRecord::regular(head, epoch, 2).with_content_refs(vec![plain.hash()]);
        medium
            .append(&sk, std::slice::from_ref(&plain), Some(head))
            .unwrap();
        let seed_value = crate::checkpoint::compute_digest_seed(&founding.id());
        let r1_bytes = medium.read_record(&r1.id()).unwrap().canonical_bytes();
        let range_digest = crate::checkpoint::chain_range_digest(&seed_value, &[r1_bytes]);
        let meta = CheckpointMetadata::new(
            sk.id(),
            r1.id(),
            vec![SkipEntry::new(Hash::ZERO, founding.id())],
            vec![
                ReclaimedIndexEntry::new(None, r1.id(), 2),
                ReclaimedIndexEntry::new(None, sk.id(), 3),
            ],
            range_digest,
            seed_value,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk =
            RevisionRecord::regular(head, epoch, 3).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(head))
            .unwrap();
        // The composite snapshot ABOVE the checkpoint, reusing the
        // shared chunk (published by R1).
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let snap =
            RevisionRecord::regular(head, epoch, 4).with_content_refs(vec![snapshot_root.hash()]);
        let reused_identity = shared_chunk.hash();
        let request = crate::composite_append::CompositeAppend {
            record: &snap,
            root: &snapshot_root,
            new_chunks: &chunks,
            reused_chunks: std::slice::from_ref(&reused_identity),
        };
        let outcome = medium.append_composite(&request, Some(head)).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        // The audit over the retirement index.
        let r1_refs = vec![shared_chunk.hash()];
        let sk_refs = vec![plain.hash()];
        let ledger = ReclamationLedger {
            mk: mk.id(),
            sk: sk.id(),
            range_digest: meta.range_digest,
            seed: meta.seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                ReclaimUnit {
                    record: sk.id(),
                    objects: sk_refs,
                },
                ReclaimUnit {
                    record: r1.id(),
                    objects: r1_refs,
                },
            ],
        };
        let (audit, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        // The provider's execution: the shared chunk is DEFERRED (a
        // survivor through the snapshot's verified manifest) and
        // physically PRESERVED.
        let outcome = medium.reclaim(&directive).unwrap();
        assert!(
            outcome.deferred.contains(&shared_chunk.hash()),
            "the provider classifies the shared chunk deferred"
        );
        assert!(medium.object_path(&shared_chunk.hash()).is_file());
        // The completion: the trait-level closure agrees — the
        // marker records the shared chunk deferred, the mandatory-
        // empty check does not refuse over the preserved chunk.
        let (marker_id, marker) =
            crate::reclaim::complete_reclamation(&mut medium, &audit, &mk.id()).unwrap();
        assert!(
            marker.deferred.contains(&shared_chunk.hash()),
            "the completion walk classifies the shared chunk deferred (parity)"
        );
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk.id()).unwrap();
        assert!(medium.object_path(&shared_chunk.hash()).is_file());
        medium.validate().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    #[ignore = "P2-R1: the proper-prefix recovery-authority ambiguity — the owner decision PENDING (Options A/B/C, none ratified); this test demonstrates the current prefix-shaped trust; the assertion flips to refusal when the owner ratifies Option A or C"]
    fn the_prefix_recovery_assumption_is_unresolved_and_proceeds_pending_the_owner_decision() {
        // (P2-R1, UNRESOLVED — NO OPTION RATIFIED) The recovery
        // policy is an open owner decision: a proper newest-first
        // deletion-prefix absence is currently accepted as
        // interruption evidence (the implementation's existing
        // behavior — NOT an owner-approved policy). WHAT IS PROVEN on this path: the exact audit identity and
        // deterministic key, the MK-bound hash-verified metadata,
        // the units-vs-index equality, the strictly-descending
        // order, the founding-derived seed, the prefix shape itself,
        // and the survivor partition protecting every
        // survivor-referenced object. WHAT IS NOT PROVEN: the range
        // digest, for any absent digest member — the review's
        // counterexample below proceeds on the unverified audit,
        // exactly as the ratified assumption accepts. The units
        // newest-first [SK, R3, R2, R1] with R1..R3 digest-covered;
        // the storage loses the PROPER prefix {SK, R3}; the audit
        // carries MATCHING FABRICATED digest values.
        let root = temp_store("r1-proper-prefix");
        let mut medium = LocalFileMedium::open(root.clone()).unwrap();
        let shared = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"shared-payload");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"private-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "p2r1", 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let p1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared.hash()]);
        medium
            .append(&p1, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        // The three digest-covered range publications plus the SK.
        let mut range_records = Vec::new();
        for tick in 2..=4u64 {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let refs = if tick == 3 {
                vec![shared.hash(), private.hash()]
            } else {
                vec![shared.hash()]
            };
            let objects: Vec<StoredObject> = if tick == 3 {
                vec![shared.clone(), private.clone()]
            } else {
                vec![shared.clone()]
            };
            let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
            medium.append(&record, &objects, Some(head)).unwrap();
            range_records.push(record);
        }
        let [r1, r2, r3] = [
            range_records[0].id(),
            range_records[1].id(),
            range_records[2].id(),
        ];
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sk = RevisionRecord::regular(head, epoch, 5).with_content_refs(vec![shared.hash()]);
        medium
            .append(&sk, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let member_bytes: Vec<Vec<u8>> = [r1, r2, r3]
            .iter()
            .map(|id| medium.read_record(id).unwrap().canonical_bytes())
            .collect();
        let _real_digest = crate::checkpoint::chain_range_digest(&seed, &member_bytes);
        let fabricated = [9; 32];
        let meta = CheckpointMetadata::new(
            sk.id(),
            r1,
            vec![SkipEntry::new(Hash::ZERO, p1.id())],
            vec![
                ReclaimedIndexEntry::new(None, r1, 2),
                ReclaimedIndexEntry::new(None, r2, 3),
                ReclaimedIndexEntry::new(None, r3, 4),
                ReclaimedIndexEntry::new(None, sk.id(), 5),
            ],
            fabricated,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk =
            RevisionRecord::regular(head, epoch, 6).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(head))
            .unwrap();
        let mut refs_of = |id: Hash| medium.read_record(&id).unwrap().content_refs.clone();
        let ledger = ReclamationLedger {
            mk: mk.id(),
            sk: sk.id(),
            range_digest: fabricated,
            seed,
            records: 4,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                ReclaimUnit {
                    record: sk.id(),
                    objects: refs_of(sk.id()),
                },
                ReclaimUnit {
                    record: r3,
                    objects: refs_of(r3),
                },
                ReclaimUnit {
                    record: r2,
                    objects: refs_of(r2),
                },
                ReclaimUnit {
                    record: r1,
                    objects: refs_of(r1),
                },
            ],
        };
        let (audit, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 7).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        // The proper-prefix loss: SK and R3 gone, R2 and R1 present.
        std::fs::remove_file(medium.object_path(&sk.id())).unwrap();
        std::fs::remove_file(medium.object_path(&r3)).unwrap();
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        // The CURRENT behavior (the unresolved policy): the deletion
        // PROCEEDS on the unverified audit — the digest is never
        // recomputed (R3 absent). This assertion flips to a typed
        // refusal when the owner ratifies Option A (strict
        // fail-closed) or Option C (new durable evidence).
        let outcome = medium.reclaim(&directive).unwrap();
        assert!(outcome.removed >= 2, "the deletion proceeded");
        assert!(!medium.object_path(&r2).is_file());
        assert!(!medium.object_path(&private.hash()).is_file());
        assert!(medium.object_path(&shared.hash()).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
    fn two_retirement_store(
        label: &str,
    ) -> (
        std::path::PathBuf,
        LocalFileMedium,
        Hash,
        Hash,
        Hash,
        Hash,
        Hash,
        crate::reclaim::ReclaimDirective,
        crate::reclaim::ReclamationLedger,
    ) {
        // The two-checkpoint campaign: F(fobj) → A1(X) → SK1(fobj)
        // → MK1 → [auditA published by the caller] → B1(X) → SK2
        // (fobj) → MK2 → [auditB published by the caller]. X is
        // shared between A's range and B's range: A defers it (B1,
        // a survivor of A, references it); B mandates it (nothing
        // surviving B references it). Returns (root, medium,
        // auditA, mk1, mk2, x, fobj, directiveA, ledgerB).
        let root = temp_store(label);
        let mut medium = LocalFileMedium::open(root.clone()).unwrap();
        let fobj = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b" founding-payload");
            enc.into_bytes()
        })
        .unwrap();
        let x = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"the-shared-deferred-x");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![fobj.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&fobj), None)
            .unwrap();
        let step = |medium: &mut LocalFileMedium, refs: Vec<Hash>, tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
            medium.append(&record, &[], Some(head)).unwrap();
            record.id()
        };
        // hmm — the append with objects must supply them; a helper
        // closure cannot borrow medium twice, so inline below.
        let _ = &step;
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
        let a1_refs = vec![x.hash()];
        let sk1_refs = vec![fobj.hash()];
        let ledger_a = ReclamationLedger {
            mk: mk1.id(),
            sk: sk1.id(),
            range_digest: digest_a,
            seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                ReclaimUnit {
                    record: sk1.id(),
                    objects: sk1_refs,
                },
                ReclaimUnit {
                    record: a1.id(),
                    objects: a1_refs,
                },
            ],
        };
        let (audit_a, outcome) = publish_reclamation_ledger(&mut medium, &ledger_a, 4).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        let directive_a = crate::reclaim::ReclaimDirective {
            audit_record: audit_a,
            units: ledger_a.units.clone(),
        };
        // The later range: B1(X) → SK2(fobj) → MK2.
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
            vec![SkipEntry::new(Hash::ZERO, audit_a)],
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
        let b1_refs = vec![x.hash()];
        let sk2_refs = vec![fobj.hash()];
        let ledger_b = ReclamationLedger {
            mk: mk2.id(),
            sk: sk2.id(),
            range_digest: digest_b,
            seed,
            records: 2,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![
                ReclaimUnit {
                    record: sk2.id(),
                    objects: sk2_refs,
                },
                ReclaimUnit {
                    record: b1.id(),
                    objects: b1_refs,
                },
            ],
        };
        (
            root,
            medium,
            audit_a,
            mk1.id(),
            mk2.id(),
            x.hash(),
            fobj.hash(),
            directive_a,
            ledger_b,
        )
    }
    #[test]
    fn a_later_audit_that_never_executed_relaxes_nothing() {
        // (P2-R2) The historical-validation counterexample: an
        // on-chain marker with an INCORRECT certified count, then a
        // LATER CANONICAL AUDIT for another checkpoint above it that
        // never executes. The audit is reclamation-SHAPED evidence,
        // not a completed retirement — it deleted nothing, so it
        // must relax nothing: the reader's full obligation equality
        // still refuses the malformed marker.
        let (root, mut medium, audit_a, mk1, mk2, _x, _fobj, directive_a, ledger_b) =
            two_retirement_store("r2-later-audit");
        medium.reclaim(&directive_a).unwrap();
        // The adversarial wrong-count marker for A, installed through
        // the in-crate admitted append.
        let head = medium.head().unwrap().unwrap();
        let parent_epoch = medium.read_record(&head).unwrap().epoch;
        let wrong = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk: mk1,
            batch: 0,
            source: audit_a,
            mandatory_count: 9,
            deferred: vec![_x, _fobj],
            journal_files: Vec::new(),
        };
        let object = StoredObject::new(wrong.canonical_bytes()).unwrap();
        let record = RevisionRecord::regular(head, parent_epoch, 99)
            .with_idempotency_key(crate::reclaim::reclamation_complete_idempotency_key(
                &mk1,
                Some(0),
            ))
            .with_content_refs(vec![object.hash()]);
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
        let outcome = medium.append_admitted(admitted, Some(head)).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        let wrong_id = record.id();
        // The later canonical audit for MK2, published ABOVE the
        // malformed marker — never executed.
        let (_audit_b, outcome) = publish_reclamation_ledger(&mut medium, &ledger_b, 8).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &wrong_id, &mk1).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/mandatory-count-mismatch"
                }
            ),
            "the never-executed later audit relaxes nothing: {error:?}"
        );
        let _ = mk2;
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_concurrent_retirement_schedule_is_safe() {
        // (P2-R4) The deterministic interleaving (the review's
        // schedule): reclamation A finished its mandatory deletion;
        // completion A verifies and observes the deferred object X;
        // the previously installed audit B authorizes X's deletion;
        // B's execution runs BETWEEN A's verification and A's CAS
        // publication (the test-only interleaving medium fires it on
        // the admitted append) — deleting X WITHOUT advancing HEAD;
        // A's marker publishes anyway. The schedule's resolution:
        // A's marker is valid (its live claims are stable; its
        // deferred list is the authorization record), the immediate
        // historical verification FAILS CLOSED (X absent, B not yet
        // completed — no accounting), and once B completes, A's
        // marker verifies through the per-object accounting (X's
        // absence is covered by B's completed retirement's own
        // audited manifest). The same test proves the legitimate
        // earlier-marker-whose-deferred-is-later-retired case and
        // the multi-generation reopen verification.
        let (root, mut medium, audit_a, mk1, _mk2, x, _fobj, directive_a, ledger_b) =
            two_retirement_store("r4-schedule");
        medium.reclaim(&directive_a).unwrap();
        assert!(medium.object_path(&x).is_file(), "X survives A (deferred)");
        // The previously installed audit for B (above A's range,
        // never yet executed).
        let (audit_b, outcome) = publish_reclamation_ledger(&mut medium, &ledger_b, 8).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        let directive_b = crate::reclaim::ReclaimDirective {
            audit_record: audit_b,
            units: ledger_b.units.clone(),
        };
        // THE INTERLEAVING: B's execution fires between A's
        // completion verification and A's marker publication.
        struct InterleavingMedium<'a> {
            inner: &'a mut LocalFileMedium,
            interleave: Option<crate::reclaim::ReclaimDirective>,
        }
        impl CanonicalMedium for InterleavingMedium<'_> {
            fn head(&mut self) -> Result<Option<Hash>, MediumError> {
                self.inner.head()
            }
            fn append_admitted(
                &mut self,
                admitted: crate::medium::AdmittedAppend,
                expected_parent: Option<Hash>,
            ) -> Result<AppendOutcome, MediumError> {
                if let Some(directive) = self.interleave.take() {
                    self.inner.reclaim(&directive)?;
                }
                self.inner.append_admitted(admitted, expected_parent)
            }
            fn read_record(&mut self, id: &Hash) -> Result<RevisionRecord, MediumError> {
                self.inner.read_record(id)
            }
            fn read_object(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
                self.inner.read_object(hash)
            }
            fn validate(&mut self) -> Result<crate::medium::ChainSummary, MediumError> {
                self.inner.validate()
            }
            fn reconcile_unknown(
                &mut self,
                candidate: &Hash,
            ) -> Result<crate::medium::Reconciliation, MediumError> {
                self.inner.reconcile_unknown(candidate)
            }
            fn durability_tier(&self) -> crate::medium::SyncTier {
                self.inner.durability_tier()
            }
            fn reclaim(
                &mut self,
                directive: &crate::reclaim::ReclaimDirective,
            ) -> Result<crate::reclaim::ReclaimOutcome, MediumError> {
                self.inner.reclaim(directive)
            }
            fn reclaim_pending(
                &mut self,
                directive: &crate::reclaim::ReclaimDirective,
            ) -> Result<bool, MediumError> {
                self.inner.reclaim_pending(directive)
            }
        }
        let mut interleaved = InterleavingMedium {
            inner: &mut medium,
            interleave: Some(directive_b.clone()),
        };
        let (marker_a, marker) =
            crate::reclaim::complete_reclamation(&mut interleaved, &audit_a, &mk1).unwrap();
        drop(interleaved);
        // B's execution ran inside the completion window: X is gone,
        // HEAD never advanced by B (reclamation deletes only).
        assert!(!medium.object_path(&x).is_file(), "B deleted X");
        assert!(
            marker.deferred.contains(&x),
            "A's marker carries X as its deferred authorization record"
        );
        // The immediate historical verification FAILS CLOSED: X is
        // absent and B has not completed (no marker, no accounting).
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/mandatory-count-mismatch"
                }
            ),
            "the mid-flight schedule fails closed (the interleaved deletion changed the recomputable partition before B completed): {error:?}"
        );
        // B completes; A's marker now verifies through the per-
        // object accounting (X's absence covered by B's completed
        // retirement's audited manifest).
        let (marker_b, marker_b_content) =
            crate::reclaim::complete_reclamation(&mut medium, &audit_b, &_mk2).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut medium, &marker_b, &_mk2).unwrap();
        assert!(!marker_b_content.deferred.contains(&x));
        // The multi-generation reopen: both historical markers verify
        // after the fresh-process restart.
        drop(medium);
        let mut reopened = LocalFileMedium::open(&root).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut reopened, &marker_a, &mk1).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut reopened, &marker_b, &_mk2).unwrap();
        reopened.validate().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn a_fabricated_later_marker_never_relaxes_an_earlier_certification() {
        // (P2-R2-H1) The adversarial cascade: a legitimate earlier
        // marker, then a LATER completion-shaped record installed
        // through the in-crate adversarial append route. The
        // fabricated later marker fails its OWN verification (the
        // cascade: the key at collection, the source audit, the
        // stable claims, its own obligation equality) — so the
        // earlier marker's verification REFUSES rather than
        // accepting the malformed evidence as attribution. Both the
        // wrong-content and the wrong-key variants refuse.
        let (root, mut medium, audit_a, mk1, _mk2, x, _fobj, directive_a, ledger_b) =
            two_retirement_store("r2h1-fabricated-later");
        medium.reclaim(&directive_a).unwrap();
        let (marker_a, _marker) =
            crate::reclaim::complete_reclamation(&mut medium, &audit_a, &mk1).unwrap();
        let (audit_b, outcome) = publish_reclamation_ledger(&mut medium, &ledger_b, 8).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        // B's execution (so the divergence the fabricated marker
        // would need to account actually exists).
        medium
            .reclaim(&crate::reclaim::ReclaimDirective {
                audit_record: audit_b,
                units: ledger_b.units.clone(),
            })
            .unwrap();
        assert!(!medium.object_path(&x).is_file());
        // The FABRICATED later marker (the wrong mandatory count)
        // via the in-crate adversarial append.
        let install = |medium: &mut LocalFileMedium,
                       marker: &crate::reclaim::ReclamationComplete,
                       valid_key: bool|
         -> Hash {
            let head = medium.head().unwrap().unwrap();
            let parent_epoch = medium.read_record(&head).unwrap().epoch;
            let key = if valid_key {
                crate::reclaim::reclamation_complete_idempotency_key(&marker.mk, Some(marker.batch))
            } else {
                b"the-wrong-key".to_vec()
            };
            let object = StoredObject::new(marker.canonical_bytes()).unwrap();
            let record = RevisionRecord::regular(head, parent_epoch, 98)
                .with_idempotency_key(key)
                .with_content_refs(vec![object.hash()]);
            let admitted =
                crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
            medium.append_admitted(admitted, Some(head)).unwrap();
            record.id()
        };
        let wrong_content = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk: _mk2,
            batch: 0,
            source: audit_b,
            mandatory_count: 42,
            deferred: vec![_fobj],
            journal_files: Vec::new(),
        };
        let _fabricated_id = install(&mut medium, &wrong_content, true);
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/later-marker-incoherent"
                }
            ),
            "the fabricated later marker refuses the cascade: {error:?}"
        );
        // The wrong-key variant refuses at collection.
        let wrong_key = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk: _mk2,
            batch: 0,
            source: audit_b,
            mandatory_count: 3,
            deferred: vec![_fobj],
            journal_files: Vec::new(),
        };
        let _wrong_key_id = install(&mut medium, &wrong_key, false);
        let error =
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/marker-key-mismatch"
                }
            ),
            "the wrong-key later marker refuses at collection: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
