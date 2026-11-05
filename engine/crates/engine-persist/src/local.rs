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
pub(crate) const MAX_OBJECT_BYTES: u64 = MAX_BYTE_LEN;

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

    /// The test-visible object-namespace path (the OC-C1 reader
    /// fixtures hand-write begin-family objects the publication
    /// gates rightly refuse — the content-addressed layout is the
    /// store's own, never a publication route).
    #[cfg(any(test, feature = "composite-test-fixture"))]
    #[doc(hidden)]
    pub fn object_path_pub(&self, hash: &Hash) -> PathBuf {
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

    fn begin_reclamation(
        &mut self,
        audit: &Hash,
        mk: &Hash,
        tick: u64,
    ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
        // (OC-C3 CP-1) The routing delegation to the accepted OC-C2
        // admission — UNCHANGED semantics, no rewiring of any
        // destructive or completion boundary.
        LocalFileMedium::begin_reclamation(self, audit, mk, tick)
    }

    fn verify_begin_authority(
        &mut self,
        audit: &Hash,
        mk: &Hash,
    ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
        // (OC-C3 CP-1) The installed-begin durability authority
        // under the exclusive HEAD.lock (the C2-R7
        // outcome-preservation discipline on unlock).
        self.lock()?;
        let result = self.verify_begin_authority_locked(audit, mk);
        let _ = self.unlock();
        result
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

    /// The OC-C2 trusted begin admission and publication under the
    /// exclusive HEAD.lock (the complete safety-critical sequence
    /// coordinated; no public trait method, no nested lock): the
    /// internal audit authentication (the exact identity, the key,
    /// the MK-bound metadata), the shared first-execution evidence
    /// core over the INTERNAL hash-verified reads (every record
    /// present including SK, the manifest equality, the INDEPENDENT
    /// digest recomputation), the OC-P2 tier gate, and the
    /// publication via append_locked with the truthful outcome
    /// classification. NOT connected to the destructive execution
    /// path (OC-C3).
    pub fn begin_reclamation(
        &mut self,
        audit: &Hash,
        mk: &Hash,
        tick: u64,
    ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
        self.lock()?;
        let outcome = self.begin_reclamation_locked(audit, mk, tick);
        // (C2-R7) The outcome-preservation discipline (the reviewed
        // append wrapper's rule): an unlock failure must never
        // replace an already established Accepted or Unknown
        // publication disposition with an ordinary definite error —
        // the OS releases the lock at handle close or process death
        // either way.
        let _ = self.unlock();
        outcome
    }

    fn begin_reclamation_locked(
        &mut self,
        audit: &Hash,
        mk: &Hash,
        tick: u64,
    ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
        // (A) The exact canonical audit identity over the internal
        // reads: the gap-aware descent, the deterministic key, the
        // strict decode, the MK binding.
        let (ledger, metadata, _head) = self.authenticate_audit_locked(audit)?;
        if ledger.mk != *mk {
            return Err(MediumError::Chain {
                rule: "begin-admission/mk-mismatch",
            });
        }
        // (F4) The gap-aware founding descent (the shared
        // reader-parameterized helper over the internal reads — a
        // missing parent is a gap crossed only under an
        // authenticated skip table, never mistaken for the
        // founding).
        let head_for_founding = self
            .read_head_file()?
            .ok_or(MediumError::Chain { rule: "unfounded" })?;
        let founding = crate::reclaim::founding_identity(
            head_for_founding,
            |identity| self.load_object(identity),
            |identity| self.load_object(identity),
            |identity| {
                // (C2-R2) The typed presence: a genuine NotFound is
                // the absence; every other filesystem error refuses —
                // never converted to a missing-parent observation.
                match std::fs::metadata(self.object_path(identity)) {
                    Ok(_) => Ok(true),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                    Err(e) => Err(io_err(
                        "founding presence probe",
                        &self.object_path(identity),
                        e,
                    )),
                }
            },
        )?;
        // (B-D) The shared first-execution evidence core over the
        // internal hash-verified object reads.
        let mut begin = crate::reclaim::verify_first_execution_evidence(
            &ledger,
            &metadata,
            &founding,
            |identity| self.load_object(identity),
            |identity| self.load_object(identity),
        )?;
        begin.source = *audit;
        // (OC-P2) The tier gate: the begin publication requires the
        // durable-authorization tier — the Windows ProcessCrashOnly
        // provider refuses fail-closed (the unrelated non-destructive
        // storage behavior untouched).
        if self.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            return Err(MediumError::Chain {
                rule: "begin-admission/tier-insufficient",
            });
        }
        // (C2-R6) The authoritative-chain validation — the SAME
        // trusted-cursor discipline the normal append applies under
        // the held lock (O(0) on the verified head, a bounded suffix
        // walk on divergence, the cold walk otherwise): a chain
        // carrying an otherwise authentic audit and retirement
        // evidence but malformed revisions above the audit never
        // receives a trusted begin — the early duplicate outcome is
        // conditional on the chain's validity, and so is the
        // installation (the internal append revalidates O(0) on the
        // now-verified head).
        self.validate_observed(self.read_head_file()?)?;
        // The duplicate resolution FIRST: a same-key begin already
        // installed is discovered and EXACTLY verified — never
        // inferred from key occupancy.
        if let Some((installed_id, installed)) = self.find_installed_begin_locked(mk)? {
            if installed == begin {
                return Ok(crate::reclaim::BeginPublicationOutcome::DuplicateVerified {
                    id: installed_id,
                });
            }
            return Err(MediumError::Chain {
                rule: "begin-admission/duplicate-content-mismatch",
            });
        }
        // (6-D) The publication: the canonical revision against the
        // authoritative HEAD, via the internal append (the lock
        // already held).
        let head = self
            .read_head_file()?
            .ok_or(MediumError::Chain { rule: "unfounded" })?;
        let parent_epoch = self.load_object(&head).map(|b| {
            RevisionRecord::decode(&b)
                .map(|r| r.epoch)
                .map_err(MediumError::Format)
        })??;
        parent_epoch.checked_add(1).ok_or(MediumError::Chain {
            rule: "epoch-regression",
        })?;
        let key = crate::reclaim::reclamation_begin_idempotency_key(&begin.mk);
        let object = StoredObject::new(begin.canonical_bytes())?;
        let record = RevisionRecord::regular(head, parent_epoch, tick)
            .with_idempotency_key(key)
            .with_content_refs(vec![object.hash()]);
        let record_bytes = record.canonical_bytes();
        let record_id = crate::hash::hash_object(&record_bytes);
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
        let outcome = self.append_admitted_locked(admitted, Some(head))?;
        match outcome {
            crate::medium::AppendOutcome::Accepted { id, tier } => {
                // (C2-R8) The ACTUAL durability tier of the accepted
                // append — propagated, never synthesized (the OC-P2
                // gate above already established FullPosixSequence;
                // ProcessCrashOnly never reaches this arm).
                Ok(crate::reclaim::BeginPublicationOutcome::Accepted { id, tier })
            }
            crate::medium::AppendOutcome::RejectedStaleParent { .. } => {
                // Under the held lock a stale parent means an
                // unexpected external mutation — the conservative
                // typed refusal (a bounded retry belongs to the
                // caller's next maintenance pass).
                Err(MediumError::Chain {
                    rule: "begin-admission/stale-parent",
                })
            }
            crate::medium::AppendOutcome::Unknown { .. } => {
                match self.reconcile_locked(&record_id)? {
                    crate::medium::Reconciliation::Installed { id } => {
                        // Membership proven; the durability
                        // acknowledgment UNCONFIRMED (the OC-C3
                        // reaffirmation upgrades it).
                        Ok(crate::reclaim::BeginPublicationOutcome::ReconciledInstalled { id })
                    }
                    crate::medium::Reconciliation::NotInstalled { .. } => {
                        Ok(crate::reclaim::BeginPublicationOutcome::ReconciledNotInstalled)
                    }
                    crate::medium::Reconciliation::Unverifiable { .. } => Err(MediumError::Chain {
                        rule: "begin-admission/reconcile-unverifiable",
                    }),
                }
            }
        }
    }

    /// (OC-C3 CP-1) The lock-held installed-begin authority
    /// verification: the recovery-safe audit authentication (no
    /// deleted original retirement record required), the
    /// founding-derived seed, the shared canonical begin verifier
    /// over the internal lock-held reads, the platform tier gate,
    /// and — only on the eligible tier — the complete reaffirmation
    /// synchronization sequence. The grade is ESTABLISHED, never
    /// assumed; every failure is typed and fail-closed.
    fn verify_begin_authority_locked(
        &self,
        audit: &Hash,
        mk: &Hash,
    ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
        let (ledger, metadata, _head) = self.authenticate_audit_locked(audit)?;
        if ledger.mk != *mk {
            return Err(MediumError::Chain {
                rule: "begin-authority/mk-mismatch",
            });
        }
        let head = self
            .read_head_file()?
            .ok_or(MediumError::Chain { rule: "unfounded" })?;
        // The founding-derived seed (the E1 discipline — the
        // recovery-safe gap-aware descent; the founding is
        // permanent, never a deleted range member).
        let founding = crate::reclaim::founding_identity(
            head,
            |identity| self.load_object(identity),
            |identity| self.load_object(identity),
            |identity| {
                // (C2-R2) The typed presence: a genuine NotFound is
                // the absence; every other filesystem error refuses.
                match fs::metadata(self.object_path(identity)) {
                    Ok(_) => Ok(true),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                    Err(e) => Err(io_err(
                        "founding presence probe",
                        &self.object_path(identity),
                        e,
                    )),
                }
            },
        )?;
        crate::reclaim::verify_founding_seed(&metadata, &founding)?;
        // The shared canonical begin verifier over ONE lock-held
        // reader (the internal non-locking hash-verified reads).
        struct LockedBeginReads<'a> {
            medium: &'a LocalFileMedium,
        }
        impl crate::reclaim::BeginReads for LockedBeginReads<'_> {
            fn read_record_bytes(&mut self, id: &Hash) -> Result<Vec<u8>, MediumError> {
                self.medium.load_object(id)
            }
            fn read_object_bytes(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
                self.medium.load_object(hash)
            }
        }
        let mut reads = LockedBeginReads { medium: self };
        let Some((id, _begin)) =
            crate::reclaim::verify_installed_begin(&mut reads, head, audit, mk, &ledger)?
        else {
            return Err(MediumError::Chain {
                rule: "begin-authority/absent",
            });
        };
        // (OC-P2) The platform tier gate: ProcessCrashOnly never
        // grants the destructive-authorization grade — an
        // installed, restored, or otherwise discoverable begin
        // cannot override the platform limitation.
        if self.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            return Err(MediumError::Chain {
                rule: "begin-authority/tier-insufficient",
            });
        }
        // The complete reaffirmation sequence — FullPosixSequence
        // only after every required operation succeeds.
        self.reaffirm_begin_authority_locked(head, id)?;
        Ok(crate::reclaim::BeginAuthority {
            id,
            tier: crate::medium::SyncTier::FullPosixSequence,
        })
    }

    /// (OC-C3 CP-1) The reaffirmation synchronization sequence under
    /// the held lock: the complete recovery-relevant
    /// authorization-evidence closure — every record on the
    /// gap-aware HEAD-to-founding ancestry and each record's first
    /// content-reference object (the checkpoint/skip metadata),
    /// which includes the begin's own record and payload object —
    /// fsynced; then the covering objects directory; then the HEAD
    /// file; then the root directory. The sequence aborts on the
    /// first failure (the typed I/O refusal — never success): an
    /// interrupted or failed reaffirmation can never return the
    /// authority grade, and (the CP-2 invariant) no deletion ever
    /// precedes a completed sequence.
    fn reaffirm_begin_authority_locked(&self, head: Hash, begin: Hash) -> Result<(), MediumError> {
        const MAX_DESCENT: u64 = 1 << 20;
        // The collecting walk: HEAD to the founding, gap-aware
        // (the same authenticated skip-table discipline), gathering
        // every walked record file and its first content-reference
        // object — exactly the future re-verification set.
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut current = head;
        let mut seen = std::collections::BTreeSet::new();
        let mut seen_begin = false;
        let mut skip_table: Option<CheckpointMetadata> = None;
        for _ in 0..MAX_DESCENT {
            if !seen.insert(current) {
                return Err(MediumError::Chain { rule: "cycle" });
            }
            let bytes = self.load_object(&current)?;
            let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
            if current == begin {
                seen_begin = true;
            }
            if let Some(object) = record.content_refs.first().copied() {
                let object_bytes = self.load_object(&object)?;
                if StoredObject::new(object_bytes.clone())?.domain()
                    == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN
                {
                    skip_table = Some(
                        CheckpointMetadata::decode(&object_bytes).map_err(MediumError::Format)?,
                    );
                }
                files.push(self.object_path(&object));
            }
            files.push(self.object_path(&current));
            let Some(parent) = record.parent else {
                break;
            };
            if !self.object_path(&parent).is_file() {
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
            } else {
                current = parent;
            }
        }
        if !seen_begin {
            // Defensive: the shared verifier already proved the
            // begin's membership; a walk that fails to reproduce it
            // is an internal inconsistency — refuse.
            return Err(MediumError::Chain {
                rule: "begin-authority/begin-not-on-ancestry",
            });
        }
        // (1) Every collected file — the records and their
        // first-reference objects.
        for path in &files {
            let file = File::open(path).map_err(|e| io_err("reaffirm open", path, e))?;
            file.sync_all()
                .map_err(|e| io_err("reaffirm sync", path, e))?;
        }
        // (OC-C3 CP-1, test-only seam) The interruption window:
        // after the file syncs, before the directory/HEAD/root
        // anchoring — an injected failure must never return
        // success.
        self.fault_pre(FaultPoint::ReaffirmFilesSynced)?;
        // (2) The covering objects directory.
        let objects_dir = self.root.join(OBJECTS_DIR);
        let dir = File::open(&objects_dir)
            .map_err(|e| io_err("reaffirm open objects dir", &objects_dir, e))?;
        dir.sync_all()
            .map_err(|e| io_err("reaffirm sync objects dir", &objects_dir, e))?;
        // (3) The HEAD file.
        let head_path = self.root.join(HEAD_FILE);
        let head_file =
            File::open(&head_path).map_err(|e| io_err("reaffirm open HEAD", &head_path, e))?;
        head_file
            .sync_all()
            .map_err(|e| io_err("reaffirm sync HEAD", &head_path, e))?;
        // (4) The root directory.
        let root =
            File::open(&self.root).map_err(|e| io_err("reaffirm open root", &self.root, e))?;
        root.sync_all()
            .map_err(|e| io_err("reaffirm sync root", &self.root, e))?;
        Ok(())
    }

    /// The begin discovery under the held lock: the descent for the
    /// deterministic key, the strict decode, the binding checks.
    fn find_installed_begin_locked(
        &self,
        mk: &Hash,
    ) -> Result<Option<(Hash, crate::reclaim::ReclamationBegin)>, MediumError> {
        const MAX_DESCENT: u64 = 1 << 20;
        let key = crate::reclaim::reclamation_begin_idempotency_key(mk);
        let mut current = self
            .read_head_file()?
            .ok_or(MediumError::Chain { rule: "unfounded" })?;
        let mut skip_table: Option<CheckpointMetadata> = None;
        for _ in 0..MAX_DESCENT {
            let bytes = self.load_object(&current)?;
            let record = RevisionRecord::decode(&bytes).map_err(MediumError::Format)?;
            if record.idempotency_key.as_deref() == Some(key.as_slice()) {
                // (F3) The full revision-shape validation — the
                // head-descent (this walk) proves the canonical
                // membership; the shape checks below complete the
                // authority proof (a keyed record is a candidate,
                // never authority by itself).
                if !matches!(record.kind, crate::record::RecordKind::Regular) {
                    return Err(MediumError::Chain {
                        rule: "begin-admission/installed-shape-mismatch",
                    });
                }
                if record.content_refs.len() != 1 {
                    return Err(MediumError::Chain {
                        rule: "begin-admission/installed-shape-mismatch",
                    });
                }
                let Some(parent) = record.parent else {
                    return Err(MediumError::Chain {
                        rule: "begin-admission/installed-shape-mismatch",
                    });
                };
                // (C2-R3) The parent/epoch linkage — the same
                // coherence the normal chain validator enforces: a
                // present parent must carry epoch+1; an absent parent
                // must be an authenticated skip-table gap.
                if self.object_path(&parent).is_file() {
                    let parent_bytes = self.load_object(&parent)?;
                    let parent_record =
                        RevisionRecord::decode(&parent_bytes).map_err(MediumError::Format)?;
                    // (C2-R7) Checked arithmetic: an epoch at u64::MAX
                    // has no valid successor — saturation must never
                    // make an incoherent linkage appear coherent.
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
                let Some(object) = record.content_refs.first().copied() else {
                    return Err(MediumError::Chain {
                        rule: "begin-admission/installed-malformed",
                    });
                };
                let object_bytes = self.load_object(&object)?;
                let begin = crate::reclaim::ReclamationBegin::decode(&object_bytes)
                    .map_err(MediumError::Format)?;
                if begin.mk != *mk {
                    return Err(MediumError::Chain {
                        rule: "begin-admission/installed-mk-mismatch",
                    });
                }
                return Ok(Some((current, begin)));
            }
            if let Some(object) = record.content_refs.first().copied() {
                let object_bytes = self.load_object(&object)?;
                if StoredObject::new(object_bytes.clone())?.domain()
                    == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN
                {
                    skip_table = Some(
                        CheckpointMetadata::decode(&object_bytes).map_err(MediumError::Format)?,
                    );
                }
            }
            let Some(parent) = record.parent else {
                return Ok(None);
            };
            if !self.object_path(&parent).is_file() {
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

    /// The internal append under the ALREADY-HELD lock: the same
    /// structural admission the public path applies before its own
    /// lock acquisition, then append_locked directly (no nested
    /// coordination).
    fn append_admitted_locked(
        &mut self,
        admitted: crate::medium::AdmittedAppend,
        expected_parent: Option<Hash>,
    ) -> Result<crate::medium::AppendOutcome, MediumError> {
        let (record, objects, composite) = admitted.parts_for_tests();
        let record = record.clone();
        let objects: Vec<StoredObject> = objects.to_vec();
        let record_bytes = record.canonical_bytes();
        let reparsed = RevisionRecord::decode(&record_bytes).map_err(MediumError::Format)?;
        if reparsed != record {
            return Err(MediumError::Chain {
                rule: "record-noncanonical",
            });
        }
        let supplied: BTreeSet<Hash> = objects.iter().map(StoredObject::hash).collect();
        let declared: BTreeSet<Hash> = record.content_refs.iter().copied().collect();
        if !composite && supplied != declared {
            return Err(MediumError::Chain {
                rule: "content-set-mismatch",
            });
        }
        let record_id = crate::hash::hash_object(&record_bytes);
        self.append_locked(
            &record,
            &objects,
            expected_parent,
            &record_bytes,
            &record_id,
            composite,
        )
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
        // (OC-C3 CP-2) THE BEGIN-BOUND DESTRUCTIVE AUTHORIZATION —
        // every authorization check precedes the FIRST irreversible
        // mutation (the cursor invalidation, PASS 1, PASS 2). The
        // platform tier FIRST: ProcessCrashOnly never grants the
        // destructive grade (regardless of any begin's existence —
        // an installed, restored, or otherwise discoverable begin
        // cannot override the platform limitation). Then the
        // installed-begin authority through the CP-1 lock-held
        // internal helper (the recovery-safe authentication, the
        // founding seed, the canonical verifier, the reaffirmation
        // — NEVER the public wrapper: no lock reacquisition under
        // the already-held HEAD.lock). The DEFINITE canonical
        // absence is the crossing's fresh-admission trigger; every
        // corrupt, mismatched, orphaned, unverifiable, or
        // insufficient-grade state is its own typed refusal — never
        // absence. A resumed retirement is never forced through the
        // OC-C2 first-execution verifier (the recovery-safe path).
        if self.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            return Err(MediumError::Chain {
                rule: "execution/begin-tier-insufficient",
            });
        }
        match self.verify_begin_authority_locked(&directive.audit_record, &ledger.mk) {
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
        // (OC-C3 CP-2) The begin-bound migration: the real begin on
        // the eligible tier; the typed platform refusal otherwise.
        if !authorize_or_refuse(
            &mut medium,
            &audit_record,
            &ledger.mk,
            &directive,
            210,
            &root,
        ) {
            return;
        }
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
        // (OC-C3 CP-2) The begin-bound migration: the real begin on
        // the eligible tier; the typed platform refusal otherwise.
        if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 202, &root) {
            return;
        }
        let outcome = medium.reclaim(&directive).unwrap();
        medium.reclaim(&directive).unwrap();
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
    fn the_local_r1_degraded_evidence_refuses_at_every_boundary() {
        // (OC-C3 CP-2 §9; CP2-R2-A) The R1 entry-point matrix at the
        // LOCAL execution boundary: the GENUINE checkpoint/audit
        // fixture (trusted_execution_store — a real campaign, a real
        // published audit) with the original retirement evidence
        // DELETED (the degradation shape) and NO begin ever
        // published. The provider deletion gate refuses the DEFINITE
        // begin absence (the distinct typed rule — the begin
        // record's canonical absence, not the missing originals);
        // the fresh admission cannot manufacture a retroactive begin
        // from the missing originals (its first-execution evidence
        // core fails closed on the absent record); no additional
        // deletion occurs (the complete object-identity snapshot is
        // unchanged across every refused attempt, the survivor AND
        // the mandatory content both still present and hash-valid);
        // and no terminal completion is published. On Windows the
        // provider gate refuses the platform tier FIRST (the
        // independent ProcessCrashOnly proof — never an ineligible
        // positive destruction) with the same unchanged-store
        // guarantee.
        let (root, mut medium, audit, mk, shared, private) = trusted_execution_store();
        let (ledger, _meta) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
        // THE DEGRADATION: every unit record deleted (the units run
        // newest-first — the deletion-prefix shape the provider's
        // own interruption discipline leaves).
        for unit in &ledger.units {
            std::fs::remove_file(medium.object_path(&unit.record)).unwrap();
        }
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        // The identity snapshot after the degradation (the baseline
        // every refused attempt must preserve exactly).
        let objects_before: std::collections::BTreeSet<String> =
            std::fs::read_dir(root.join("objects"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
        // (a) THE PROVIDER DELETION GATE.
        let error = medium.reclaim(&directive).unwrap_err();
        if cfg!(windows) {
            // The platform tier refuses FIRST — the independent
            // ProcessCrashOnly proof.
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "execution/begin-tier-insufficient"
                    }
                ),
                "the Windows provider gate refuses the platform tier: {error:?}"
            );
            let objects_after: std::collections::BTreeSet<String> =
                std::fs::read_dir(root.join("objects"))
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
            assert_eq!(objects_after, objects_before, "nothing changed");
            assert!(
                crate::reclaim::find_installed_completion(&mut medium, &mk)
                    .unwrap()
                    .is_none()
            );
            drop(medium);
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "execution/begin-absent"
                }
            ),
            "the provider gate refuses the definite begin absence: {error:?}"
        );
        // (b) THE FRESH ADMISSION: the first-execution evidence core
        // fails closed on the missing originals — no retroactive
        // begin is manufactured from the degradation.
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
        // The gate still refuses after the failed admission.
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
        // NO ADDITIONAL DELETION: the identity snapshot is unchanged
        // across all three refused attempts, and both the survivor
        // content and the mandatory content remain present and
        // hash-valid (the provider's reads verify the hash).
        medium.read_object(&shared).unwrap();
        medium.read_object(&private).unwrap();
        let objects_after: std::collections::BTreeSet<String> =
            std::fs::read_dir(root.join("objects"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
        assert_eq!(objects_after, objects_before, "no additional deletion");
        // NO TERMINAL COMPLETION.
        assert!(
            crate::reclaim::find_installed_completion(&mut medium, &mk)
                .unwrap()
                .is_none(),
            "no completion marker was published"
        );
        drop(medium);
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
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        // (OC-C3 CP-2) The begin-bound migration (BEFORE the crash
        // injection — the R4-F5 rule): the real begin on the
        // eligible tier; the typed platform refusal otherwise.
        if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 201, &root) {
            return;
        }
        // The crash shape (injected AFTER the admission): the FIRST
        // unit's record is gone; its objects remain.
        let interrupted_record = ledger.units[0].record;
        std::fs::remove_file(medium.object_path(&interrupted_record)).unwrap();
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
            // (OC-C3 CP-2) The fabricated state can never be
            // AUTHORIZED — the admission's own evidence core refuses
            // it (the property one boundary earlier); the execution
            // gate refuses the definite begin absence.
            // (OC-C3 CP-2) On ProcessCrashOnly the admission refuses
            // the platform tier before the evidence core — the gate
            // refuses the destructive grade; the fabricated-state
            // property is covered on POSIX and the SQLite suites.
            if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
                let error = medium.reclaim(&directive).unwrap_err();
                assert!(
                    matches!(
                        error,
                        MediumError::Chain {
                            rule: "execution/begin-tier-insufficient"
                        }
                    ),
                    "ProcessCrashOnly refuses the destructive grade: {error:?}"
                );
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
            let admit = medium.begin_reclamation(&audit, &mk, 213).unwrap_err();
            let error = medium.reclaim(&directive).unwrap_err();
            assert!(
                matches!(
                    admit,
                    MediumError::Chain {
                        rule: "begin-admission/range-digest-mismatch"
                    }
                ),
                "the fabricated digest refuses at the admission's evidence core: {admit:?}"
            );
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "execution/begin-absent"
                    }
                ),
                "the unauthorized state refuses at the execution gate: {error:?}"
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
            // (OC-C3 CP-2) The E2 order check PRECEDES the
            // begin-bound gate in the reclaim's own read-only
            // sequence — the original deeper boundary still reached
            // unchanged (the property preserved at its exact
            // boundary).
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
            // (OC-C3 CP-2) The begin-bound migration: the real begin on
            // the eligible tier; the typed platform refusal otherwise.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 205, &root) {
                return;
            }
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

    /// (OC-C3 CP-2) The shared positive-path migration: on the
    /// eligible tier publish the REAL trusted begin (the accepted
    /// admission — never a direct installation); on a
    /// ProcessCrashOnly platform assert the typed execution refusal
    /// (BEFORE any deletion — every retiring record still present)
    /// and signal the early return. Returns false when the test
    /// should stop (the platform refusal path — already cleaned up).
    fn authorize_or_refuse(
        medium: &mut LocalFileMedium,
        audit: &Hash,
        mk: &Hash,
        directive: &crate::reclaim::ReclaimDirective,
        tick: u64,
        root: &std::path::Path,
    ) -> bool {
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.reclaim(directive).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "execution/begin-tier-insufficient"
                    }
                ),
                "ProcessCrashOnly refuses the destructive grade before deletion: {error:?}"
            );
            for unit in &directive.units {
                assert!(
                    medium.object_path_pub(&unit.record).is_file(),
                    "no retiring record deleted by the refusal"
                );
            }
            let _ = std::fs::remove_dir_all(root);
            return false;
        }
        let outcome = medium.begin_reclamation(audit, mk, tick).unwrap();
        assert!(
            matches!(
                outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "the fixture's begin admits: {outcome:?}"
        );
        true
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
        // (OC-C3 CP-2) The begin-bound migration: the real begin on
        // the eligible tier; the typed platform refusal otherwise.
        if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 200, &root) {
            return;
        }
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
        // (OC-C3 CP-2) The begin-bound migration: the real begin on
        // the eligible tier; the typed platform refusal otherwise.
        if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 203, &root) {
            return;
        }
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
            // (OC-C3 CP-2) The begin-bound migration.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 211, &root) {
                return;
            }
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
            // (OC-C3 CP-2) The begin-bound migration.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 211, &root) {
                return;
            }
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
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            // (OC-C4) The begin-bound migration: the retirement's
            // begin is ADMITTED but never executed — the marker's
            // verification passes the begin authority on POSIX and
            // refuses at the unexecuted retirement's still-present
            // records (the original deeper boundary preserved); on
            // Windows the admission's tier refusal is the platform
            // proof (no destructive authorization exists to
            // certify).
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 213, &root) {
                return;
            }
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
            // (OC-C3 CP-2) The begin-bound migration.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 211, &root) {
                return;
            }
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
        // (OC-C3 CP-2) The begin-bound migration: on the eligible
        // tier the REAL admission; on ProcessCrashOnly the typed
        // execution refusal (nothing deleted — the composite-closure
        // substance covered on POSIX and the SQLite suites).
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.reclaim(&directive).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "execution/begin-tier-insufficient"
                    }
                ),
                "ProcessCrashOnly refuses the destructive grade: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let begin_outcome = medium.begin_reclamation(&audit, &mk.id(), 245).unwrap();
        assert!(
            matches!(
                begin_outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "the fixture's begin admits: {begin_outcome:?}"
        );
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
    /// (P2-R1 Option C) The proper-prefix campaign: F(shared) →
    /// P1(shared) → R1(shared) → R2(shared) → R3(shared+private) →
    /// SK(shared) → MK (the index [R1, R2, R3, SK], the range
    /// digest over R1..R3 — the REAL recomputation, or the
    /// fabricated [9; 32] for the adversarial variant) → the audit.
    /// The units run newest-first [SK, R3, R2, R1]; the shared
    /// object is the survivor (P1 references it), the private
    /// object is R3-mandated. Returns (root, medium, audit, mk,
    /// directive, sk, r3, r2, r1, shared, private).
    fn p2r1_prefix_campaign(
        label: &str,
        fabricate_digest: bool,
    ) -> (
        std::path::PathBuf,
        LocalFileMedium,
        Hash,
        Hash,
        crate::reclaim::ReclaimDirective,
        Hash,
        Hash,
        Hash,
        Hash,
        Hash,
        Hash,
    ) {
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
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
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
        let real_digest = crate::checkpoint::chain_range_digest(&seed, &member_bytes);
        let digest = if fabricate_digest {
            [9u8; 32]
        } else {
            real_digest
        };
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
            digest,
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
            range_digest: digest,
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
        let directive = crate::reclaim::ReclaimDirective {
            audit_record: audit,
            units: ledger.units.clone(),
        };
        (
            root,
            medium,
            audit,
            mk.id(),
            directive,
            sk.id(),
            r3,
            r2,
            r1,
            shared.hash(),
            private.hash(),
        )
    }

    #[test]
    fn the_p2r1_beginless_prefix_refuses_recovery_without_a_begin() {
        // (P2-R1, OPTION C RATIFIED — the negative case) A
        // proper-prefix deletion absence is not, by itself,
        // authorization to continue destructive work: the genuine
        // audit with the proper-prefix loss {SK, R3} and NO begin
        // ever admitted refuses at the provider's execution gate
        // (the definite begin absence); the fresh first admission
        // cannot manufacture a retroactive begin from the missing
        // originals (its first-execution evidence core fails
        // closed); the remaining evidence is unchanged; and no
        // terminal marker appears. On Windows the platform's typed
        // insufficient-tier refusal fires first (the independent
        // ProcessCrashOnly proof).
        let (root, mut medium, audit, mk, directive, sk, r3, _r2, _r1, _shared, _private) =
            p2r1_prefix_campaign("p2r1-beginless", false);
        // The proper-prefix loss (the genuine newest-first
        // interruption shape).
        std::fs::remove_file(medium.object_path(&sk)).unwrap();
        std::fs::remove_file(medium.object_path(&r3)).unwrap();
        // The remaining-evidence baseline.
        let objects_before: std::collections::BTreeSet<String> =
            std::fs::read_dir(root.join("objects"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
        // THE PROVIDER REFUSAL.
        let error = medium.reclaim(&directive).unwrap_err();
        if cfg!(windows) {
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "execution/begin-tier-insufficient"
                    }
                ),
                "the Windows platform tier refuses the recovery: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "execution/begin-absent"
                }
            ),
            "the beginless prefix never authorizes recovery: {error:?}"
        );
        // THE FAILED FIRST ADMISSION: the original evidence is
        // unavailable (SK gone) — no retroactive begin is
        // manufactured.
        let error = medium.begin_reclamation(&audit, &mk, 240).unwrap_err();
        assert!(
            matches!(error, MediumError::MissingObject { .. }),
            "the first admission fails closed on the missing originals: {error:?}"
        );
        // The gate still refuses after the failed admission.
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
        // The remaining evidence unchanged; no terminal marker.
        let objects_after: std::collections::BTreeSet<String> =
            std::fs::read_dir(root.join("objects"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
        assert_eq!(objects_after, objects_before, "nothing changed");
        assert!(
            crate::reclaim::find_installed_completion(&mut medium, &mk)
                .unwrap()
                .is_none(),
            "no terminal marker was published"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_p2r1_authorized_prefix_interruption_recovers_under_the_admitted_begin() {
        // (P2-R1, OPTION C RATIFIED — the positive case) Recovery
        // across a proper-prefix deletion interruption is permitted
        // under a valid, previously admitted, durably installed
        // ReclamationBegin: the REAL trusted admission runs while
        // every first-execution evidence member is intact; the
        // genuine proper newest-first prefix {SK, R3} is then lost;
        // a fresh provider handle (the restart) resumes the
        // retirement under the independently rediscovered,
        // authenticated begin — the correct durability grade, NO
        // second begin, convergence to the completed state (the
        // marker through the trusted wrapper, verified by the
        // OC-C4 reader), the survivor protection, and the hash
        // integrity. On Windows the admission itself refuses the
        // platform tier (the independent proof — the recovery
        // substance is POSIX-covered).
        let (root, mut medium, audit, mk, directive, sk, r3, _r2, _r1, shared, _private) =
            p2r1_prefix_campaign("p2r1-authorized", false);
        // THE REAL ADMISSION (every first-execution evidence member
        // intact).
        let begin = medium.begin_reclamation(&audit, &mk, 240);
        if cfg!(windows) {
            match begin {
                Err(MediumError::Chain {
                    rule: "begin-admission/tier-insufficient",
                }) => {}
                other => panic!("the Windows admission refuses the platform tier: {other:?}"),
            }
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let crate::reclaim::BeginPublicationOutcome::Accepted { id: begin_id, .. } =
            begin.expect("the real admission accepts")
        else {
            unreachable!("the accepted pattern is matched above")
        };
        // THE GENUINE PROPER PREFIX (the newest-first deletion
        // interruption shape).
        std::fs::remove_file(medium.object_path(&sk)).unwrap();
        std::fs::remove_file(medium.object_path(&r3)).unwrap();
        // THE RECOVERY: a fresh provider handle (the restart), the
        // resume under the installed begin.
        drop(medium);
        let mut recovered = LocalFileMedium::open(root.clone()).unwrap();
        let outcome = recovered
            .reclaim(&directive)
            .expect("the authorized interruption resumes and completes");
        assert!(outcome.removed >= 1, "the remaining work executed");
        // NO SECOND BEGIN: the authority independently rediscovers
        // the SAME record, at the correct durability grade.
        let authority = recovered.verify_begin_authority(&audit, &mk).unwrap();
        assert_eq!(authority.id, begin_id, "no second begin was published");
        assert_eq!(
            authority.tier,
            crate::medium::SyncTier::FullPosixSequence,
            "the durability grade"
        );
        // THE CONVERGENCE: the completion through the trusted
        // wrapper (which itself requires the begin), verified by
        // the OC-C4 completion reader.
        let (marker_id, _marker) =
            crate::reclaim::complete_reclamation(&mut recovered, &audit, &mk).unwrap();
        crate::reclaim::verify_reclamation_complete(&mut recovered, &marker_id, &mk).unwrap();
        // THE SURVIVOR PROTECTION + the hash integrity + the
        // idempotent re-run.
        assert!(
            recovered.object_path(&shared).is_file(),
            "the survivor-referenced shared object is preserved"
        );
        recovered.validate().unwrap();
        let rerun = recovered
            .reclaim(&directive)
            .expect("the completed retirement re-runs idempotently");
        assert_eq!(rerun.removed, 0, "nothing left to delete");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_p2r1_fabricated_digest_refuses_trusted_first_admission() {
        // (P2-R1, OPTION C RATIFIED — the fabricated-digest case) A
        // fabricated checkpoint/audit digest cannot obtain trusted
        // first-execution begin admission while the intact original
        // evidence contradicts it: the admission's independent
        // range-digest recomputation over the actual, hash-verified
        // canonical record bytes refuses (the original history
        // itself must prove the commitment — a ledger whose digest
        // merely agrees with the metadata is never enough); with no
        // begin admitted, the execution gate refuses the definite
        // begin absence; the intact evidence is unchanged and no
        // terminal marker appears. On Windows the platform tier
        // refuses first (the independent proof).
        let (root, mut medium, audit, mk, directive, sk, r3, r2, r1, _shared, _private) =
            p2r1_prefix_campaign("p2r1-fabricated", true);
        // THE ADMISSION REFUSES (the intact evidence contradicts the
        // fabricated commitment).
        let error = medium.begin_reclamation(&audit, &mk, 240).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/range-digest-mismatch"
                }
            ),
            "the fabricated digest cannot be authorized: {error:?}"
        );
        // The gate refuses (no begin was admitted).
        let error = medium.reclaim(&directive).unwrap_err();
        if cfg!(windows) {
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "execution/begin-tier-insufficient"
                    }
                ),
                "the Windows platform tier refuses the recovery: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "execution/begin-absent"
                }
            ),
            "the beginless fabricated state never authorizes recovery: {error:?}"
        );
        // The intact evidence unchanged; no terminal marker.
        assert!(medium.object_path(&sk).is_file());
        assert!(medium.object_path(&r3).is_file());
        assert!(medium.object_path(&r2).is_file());
        assert!(medium.object_path(&r1).is_file());
        assert!(
            crate::reclaim::find_installed_completion(&mut medium, &mk)
                .unwrap()
                .is_none(),
            "no terminal marker was published"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (V2-W) The plan campaign: F(shared) → P1(shared) → R1(shared)
    /// → R2(shared+private) → SK(shared) → MK — the index [R1, R2,
    /// SK], the range digest over R1..R2 oldest-first, and NO v1
    /// audit (the checkpoint is plan-eligible). The shared object
    /// repeats across the R1/R2/SK units (the ratified cross-unit
    /// sharing) and is the survivor (P1 references it); the private
    /// object is R2-mandated. Returns (root, journal_root, medium,
    /// mk, r1, r2, shared, private).
    fn v2_plan_campaign(
        label: &str,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        LocalFileMedium,
        Hash,
        Hash,
        Hash,
        Hash,
        Hash,
    ) {
        let root = temp_store(label);
        let journal_root = std::env::temp_dir().join(format!(
            "s7-v2w-journal-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut medium = LocalFileMedium::open(root.clone()).unwrap();
        let shared = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"v2w-shared-payload");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"v2w-private-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        // (V2-W F4, the F4-R6 fresh shape) The bound bootstrap AT THE
        // FOUNDING — the production ordering (the crossing's
        // open_bound_bootstrap): the durable bound journal at the
        // root, then the canonical root binding published while the
        // founding is the chain's head (the first binding's
        // fresh-bootstrap admission — the campaign's journal
        // authority, the plan publication's prerequisite), and only
        // then the campaign's revisions on top of the binding.
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
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let p1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared.hash()]);
        medium
            .append(&p1, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let r1 = RevisionRecord::regular(head, epoch, 2).with_content_refs(vec![shared.hash()]);
        medium
            .append(&r1, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let r2 = RevisionRecord::regular(head, epoch, 3)
            .with_content_refs(vec![shared.hash(), private.hash()]);
        medium
            .append(&r2, &[shared.clone(), private.clone()], Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sk = RevisionRecord::regular(head, epoch, 4).with_content_refs(vec![shared.hash()]);
        medium
            .append(&sk, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let member_bytes: Vec<Vec<u8>> = [r1.id(), r2.id()]
            .iter()
            .map(|id| medium.read_record(id).unwrap().canonical_bytes())
            .collect();
        let digest = crate::checkpoint::chain_range_digest(&seed, &member_bytes);
        let meta = CheckpointMetadata::new(
            sk.id(),
            r1.id(),
            vec![SkipEntry::new(Hash::ZERO, p1.id())],
            vec![
                ReclaimedIndexEntry::new(None, r1.id(), 2),
                ReclaimedIndexEntry::new(None, r2.id(), 3),
                ReclaimedIndexEntry::new(None, sk.id(), 4),
            ],
            digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk =
            RevisionRecord::regular(head, epoch, 5).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(head))
            .unwrap();
        (
            root,
            journal_root,
            medium,
            mk.id(),
            r1.id(),
            r2.id(),
            shared.hash(),
            private.hash(),
        )
    }

    /// (V2-W, restored by F4-R4) The parameterized plan-campaign
    /// variant — the same bound-bootstrapped shape as
    /// [`v2_plan_campaign`] with the adversarial controls the
    /// restored F1/F2/F3/F5B tests need: the range digest (the
    /// fabricated [9; 32] or the genuine recomputation), the seed
    /// (the genuine founding-derived or the fabricated), the index
    /// epoch order (the genuine or the swapped mis-order), and R2's
    /// extra direct references (the unit-size control). The bound
    /// bootstrap runs AT THE FOUNDING (the F4-R6 fresh shape — the
    /// production ordering). Returns (root, journal_root, medium,
    /// mk, r1, r2, shared, private).
    #[allow(clippy::type_complexity)]
    fn v2_plan_campaign_variant(
        label: &str,
        fabricate_digest: bool,
        wrong_seed: bool,
        swap_index_epochs: bool,
        r2_extra_objects: usize,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        LocalFileMedium,
        Hash,
        Hash,
        Hash,
        Hash,
        Hash,
    ) {
        let root = temp_store(label);
        let journal_root = std::env::temp_dir().join(format!(
            "s7-v2wfv-journal-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut medium = LocalFileMedium::open(root.clone()).unwrap();
        let shared = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"v2wfv-shared-payload");
            enc.into_bytes()
        })
        .unwrap();
        let private = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"v2wfv-private-payload");
            enc.into_bytes()
        })
        .unwrap();
        let mut extra_objects = Vec::new();
        for index in 0..r2_extra_objects {
            extra_objects.push(
                StoredObject::new({
                    let mut enc = engine_core::Encoder::new();
                    enc.write_str("sektor7/save/1");
                    enc.write_bytes(format!("v2wfv-extra-{index}").as_bytes());
                    enc.into_bytes()
                })
                .unwrap(),
            );
        }
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        // (V2-W F4, the F4-R6 fresh shape) The bound bootstrap AT THE
        // FOUNDING — the production ordering (the crossing's
        // open_bound_bootstrap): the durable bound journal at the
        // root, then the canonical root binding published while the
        // founding is the chain's head, then the campaign's
        // revisions on top of the binding.
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
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let p1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared.hash()]);
        medium
            .append(&p1, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let r1 = RevisionRecord::regular(head, epoch, 2).with_content_refs(vec![shared.hash()]);
        medium
            .append(&r1, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mut r2_refs = vec![shared.hash(), private.hash()];
        let mut r2_objects = vec![shared.clone(), private.clone()];
        for object in &extra_objects {
            r2_refs.push(object.hash());
            r2_objects.push(object.clone());
        }
        let r2 = RevisionRecord::regular(head, epoch, 3).with_content_refs(r2_refs);
        medium.append(&r2, &r2_objects, Some(head)).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sk = RevisionRecord::regular(head, epoch, 4).with_content_refs(vec![shared.hash()]);
        medium
            .append(&sk, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let real_seed = crate::checkpoint::compute_digest_seed(&founding.id());
        let member_bytes: Vec<Vec<u8>> = [r1.id(), r2.id()]
            .iter()
            .map(|id| medium.read_record(id).unwrap().canonical_bytes())
            .collect();
        let real_digest = crate::checkpoint::chain_range_digest(&real_seed, &member_bytes);
        let digest = if fabricate_digest {
            [9u8; 32]
        } else {
            real_digest
        };
        let seed = if wrong_seed {
            crate::checkpoint::compute_digest_seed(&Hash::ZERO)
        } else {
            real_seed
        };
        let mut entries = vec![
            ReclaimedIndexEntry::new(None, r1.id(), 2),
            ReclaimedIndexEntry::new(None, r2.id(), 3),
            ReclaimedIndexEntry::new(None, sk.id(), 4),
        ];
        if swap_index_epochs {
            // The adversarial incoherence: the CLAIMED epochs swapped
            // between the members (r1 claims 3, r2 claims 2) — the
            // metadata's ordering claim contradicts the original
            // records' actual positions, surfacing at the independent
            // digest recomputation (the claimed order reorders the
            // digest members).
            entries[0] = ReclaimedIndexEntry::new(None, r1.id(), 3);
            entries[1] = ReclaimedIndexEntry::new(None, r2.id(), 2);
        }
        let meta = CheckpointMetadata::new(
            sk.id(),
            r1.id(),
            vec![SkipEntry::new(Hash::ZERO, p1.id())],
            entries,
            digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk =
            RevisionRecord::regular(head, epoch, 5).with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(head))
            .unwrap();
        (
            root,
            journal_root,
            medium,
            mk.id(),
            r1.id(),
            r2.id(),
            shared.hash(),
            private.hash(),
        )
    }

    #[test]
    fn the_v2_plan_publication_admission_matrix() {
        // (V2-W §9 A/C/H/I) The trusted plan admission: the valid
        // publication under the deterministic MK-bound key with the
        // COMPLETE membership partition; the invalid MK and byte
        // limits; the exact-duplicate idempotence and the
        // conflicting-content refusal; the restart discovery; and
        // the non-destruction proof (only the plan's record and
        // object added — nothing deleted, no begin, no completion
        // marker, the survivor and mandatory content intact).
        let (root, journal_root, mut medium, mk, r1, r2, shared, private) =
            v2_plan_campaign("v2w-admission");
        let records_before = medium.validate().unwrap().records;
        // THE INVALID MK.
        let error = crate::reclaim::publish_reclamation_plan(
            &mut medium,
            &Hash::ZERO,
            1024,
            &journal_root,
            50,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-plan/mk-not-installed"
                }
            ),
            "an unknown MK refuses: {error:?}"
        );
        // THE INVALID BYTE LIMITS.
        for limit in [0, crate::reclaim::MAX_RECLAMATION_LEDGER_BYTES + 1] {
            let error = crate::reclaim::publish_reclamation_plan(
                &mut medium,
                &mk,
                limit,
                &journal_root,
                50,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-plan/ledger-byte-limit-invalid"
                    }
                ),
                "the invalid byte limit refuses: {error:?}"
            );
        }
        // THE VALID PUBLICATION (the small campaign fits one batch).
        let (plan_id, outcome) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 51)
                .unwrap();
        assert!(
            matches!(
                outcome,
                crate::reclaim::PlanPublicationOutcome::Accepted { id } if id == plan_id
            ),
            "the valid plan publishes: {outcome:?}"
        );
        // The discovery + the complete membership partition (the
        // index exactly once, the ratified epoch order newest-first).
        let (found_id, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
            .unwrap()
            .unwrap();
        assert_eq!(found_id, plan_id);
        assert_eq!(plan.mk, mk);
        assert_eq!(plan.batches.len(), 1);
        let members = &plan.batches[0].records;
        assert_eq!(members.len(), 3, "the complete index membership");
        assert!(members.contains(&r1) && members.contains(&r2));
        // THE EXACT DUPLICATE (idempotent under the deterministic
        // key).
        let (dup_id, dup_outcome) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 52)
                .unwrap();
        assert!(
            matches!(
                dup_outcome,
                crate::reclaim::PlanPublicationOutcome::DuplicateVerified { id } if id == plan_id
            ),
            "the exact duplicate verifies: {dup_outcome:?}"
        );
        assert_eq!(dup_id, plan_id);
        // THE CONFLICTING CONTENT (a different limit → a different
        // partition → the same key, different bytes → refuse, never
        // an equivalent duplicate).
        let error =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 2048, &journal_root, 53)
                .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-plan/duplicate-content-mismatch"
                }
            ),
            "the conflicting content refuses: {error:?}"
        );
        // THE RESTART DISCOVERY (a fresh provider handle — the
        // installed records and identities verified without
        // re-deriving them from caller state).
        drop(medium);
        let mut reopened = LocalFileMedium::open(root.clone()).unwrap();
        let (restart_id, restart_plan) = crate::reclaim::find_installed_plan(&mut reopened, &mk)
            .unwrap()
            .unwrap();
        assert_eq!(restart_id, plan_id);
        assert_eq!(restart_plan, plan);
        // THE NON-DESTRUCTION: exactly the plan's record added;
        // every retirement record and object intact; no completion
        // marker.
        let summary = reopened.validate().unwrap();
        assert_eq!(
            summary.records,
            records_before + 1,
            "only the plan's revision was added"
        );
        assert!(reopened.object_path(&r1).is_file());
        assert!(reopened.object_path(&r2).is_file());
        assert!(reopened.object_path(&shared).is_file());
        assert!(reopened.object_path(&private).is_file());
        assert!(
            crate::reclaim::find_installed_completion(&mut reopened, &mk)
                .unwrap()
                .is_none(),
            "no completion marker was published"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_w2_exclusion_fence_and_competing_writers() {
        // (V2-W §9 B/J) The W2 bidirectional v1/v2 exclusion at every
        // reachable installation path: a v1-audited MK refuses the
        // plan admission; a planned MK refuses the v1 publication;
        // the unrestricted generic append refuses BOTH families; and
        // the competing-writer races over two provider handles never
        // leave a chain carrying conflicting trusted retirement
        // authority (the same plan converges idempotently; a
        // conflicting plan refuses; the cross-type publication
        // refuses through the fresh exclusion check anchored to the
        // CAS head).
        let v1_ledger = |medium: &mut LocalFileMedium, mk: Hash| {
            let mk_record = medium.read_record(&mk).unwrap();
            let meta_bytes = medium.read_object(&mk_record.content_refs[0]).unwrap();
            let metadata = CheckpointMetadata::decode(&meta_bytes).unwrap();
            let mut refs_of = |id: Hash| medium.read_record(&id).unwrap().content_refs.clone();
            ReclamationLedger {
                mk,
                sk: metadata.sk,
                range_digest: metadata.range_digest,
                seed: metadata.seed,
                records: metadata.index.len() as u64,
                reclaimed_keys: 0,
                journal_files: Vec::new(),
                units: metadata
                    .index
                    .iter()
                    .map(|entry| ReclaimUnit {
                        record: entry.hash,
                        objects: refs_of(entry.hash),
                    })
                    .collect(),
            }
        };
        // DIRECTION 1: a v1-audited MK refuses the plan.
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
                v2_plan_campaign("v2w-w2-v1first");
            let ledger = v1_ledger(&mut medium, mk);
            let (_audit, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 60).unwrap();
            assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
            let error =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 61)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-plan/v1-audit-exclusion"
                    }
                ),
                "the v1-audited MK refuses the plan: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // DIRECTION 2: a planned MK refuses the v1 publication.
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
                v2_plan_campaign("v2w-w2-planfirst");
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 62)
                .unwrap();
            let ledger = v1_ledger(&mut medium, mk);
            let error = publish_reclamation_ledger(&mut medium, &ledger, 63).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-ledger/v2-plan-exclusion"
                    }
                ),
                "the planned MK refuses the v1 publication: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // THE GENERIC-APPEND BYPASS: hand-built ledger and plan
        // objects refused by the unrestricted append (the family
        // gates).
        {
            let (root, _journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
                v2_plan_campaign("v2w-w2-bypass");
            let ledger = v1_ledger(&mut medium, mk);
            let ledger_object = StoredObject::new(ledger.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, 64)
                .with_content_refs(vec![ledger_object.hash()]);
            let error = medium
                .append(&record, std::slice::from_ref(&ledger_object), Some(head))
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-ledger/generic-append-refused"
                    }
                ),
                "the generic append refuses the ledger family: {error:?}"
            );
            let plan_object = StoredObject::new(
                crate::reclaim::ReclaimPlan {
                    mk,
                    sk: Hash::ZERO,
                    range_digest: [0u8; 32],
                    seed: [0u8; 32],
                    ledger_byte_limit: 1024,
                    batches: Vec::new(),
                    journal_files: Vec::new(),
                }
                .canonical_bytes(),
            )
            .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, 65)
                .with_content_refs(vec![plan_object.hash()]);
            let error = medium
                .append(&record, std::slice::from_ref(&plan_object), Some(head))
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-plan/generic-append-refused"
                    }
                ),
                "the generic append refuses the plan family: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // THE COMPETING WRITERS (two provider handles over the same
        // root): the same plan converges idempotently; a conflicting
        // plan refuses; the cross-type publication refuses through
        // the fresh exclusion check.
        {
            let (root, journal_root, medium_a, mk, _r1, _r2, _shared, _private) =
                v2_plan_campaign("v2w-w2-race");
            drop(medium_a);
            let mut a = LocalFileMedium::open(root.clone()).unwrap();
            let mut b = LocalFileMedium::open(root.clone()).unwrap();
            let (id_a, out_a) =
                crate::reclaim::publish_reclamation_plan(&mut a, &mk, 1024, &journal_root, 66)
                    .unwrap();
            assert!(
                matches!(
                    out_a,
                    crate::reclaim::PlanPublicationOutcome::Accepted { .. }
                ),
                "{out_a:?}"
            );
            let (id_b, out_b) =
                crate::reclaim::publish_reclamation_plan(&mut b, &mk, 1024, &journal_root, 67)
                    .unwrap();
            assert!(
                matches!(
                    out_b,
                    crate::reclaim::PlanPublicationOutcome::DuplicateVerified { id } if id == id_a
                ),
                "the competing same-plan writer converges idempotently: {out_b:?}"
            );
            assert_eq!(id_a, id_b);
            let error =
                crate::reclaim::publish_reclamation_plan(&mut b, &mk, 2048, &journal_root, 68)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-plan/duplicate-content-mismatch"
                    }
                ),
                "the conflicting competing plan refuses: {error:?}"
            );
            let ledger = v1_ledger(&mut b, mk);
            let error = publish_reclamation_ledger(&mut b, &ledger, 69).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-ledger/v2-plan-exclusion"
                    }
                ),
                "the competing cross-type writer refuses: {error:?}"
            );
            // No final chain carries conflicting trusted authority:
            // the plan is installed, the v1 audit is not.
            assert!(
                crate::reclaim::find_installed_plan(&mut b, &mk)
                    .unwrap()
                    .is_some()
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_v2_batch_audit_publication_matrix() {
        // (V2-W §9 D/E/H/I) The trusted v2 batch-audit publication:
        // every plan batch publishes under its deterministic (MK,
        // batch) key with the exact membership slice and the
        // ratified sharing; the unknown batch index refuses; the
        // missing evidence and corrupt objects refuse through the
        // hash-verified reads; the Unknown publication outcomes
        // reconcile with the verified identity, key, and exact
        // content; the restart discovers the installed audits; and
        // the publications are non-destructive.
        let (root, journal_root, mut medium, mk, r1, r2, shared, _private) =
            v2_plan_campaign("v2w-audits");
        let records_before = medium.validate().unwrap().records;
        let (plan_id, _) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 70)
                .unwrap();
        let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
            .unwrap()
            .unwrap();
        // EVERY batch (the all-audits-first discipline as a
        // property: the complete set publishable and discoverable).
        let mut audit_ids = Vec::new();
        for batch in plan.batches.iter().map(|b| b.index) {
            let (audit_id, outcome) =
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, batch, 71).unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::LedgerV2PublicationOutcome::Accepted { id } if id == audit_id
                ),
                "the batch audit publishes: {outcome:?}"
            );
            // The discovery + the membership exactness.
            let (found_id, ledger) =
                crate::reclaim::find_installed_v2_audit(&mut medium, &mk, batch)
                    .unwrap()
                    .unwrap();
            assert_eq!(found_id, audit_id);
            assert_eq!(ledger.mk, mk);
            assert_eq!(ledger.batch, batch);
            let slice = &plan.batches[batch as usize];
            assert_eq!(ledger.units.len(), slice.records.len());
            for (unit, record) in ledger.units.iter().zip(&slice.records) {
                assert_eq!(unit.record, *record);
            }
            // The ratified cross-unit sharing: the shared object
            // appears in multiple units legitimately.
            assert!(
                ledger
                    .units
                    .iter()
                    .filter(|unit| unit.objects.contains(&shared))
                    .count()
                    >= 2,
                "the cross-unit repetition is legitimate sharing"
            );
            // The exact duplicate.
            let (_dup_id, dup_outcome) =
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, batch, 72).unwrap();
            assert!(
                matches!(
                    dup_outcome,
                    crate::reclaim::LedgerV2PublicationOutcome::DuplicateVerified { id } if id == audit_id
                ),
                "the exact duplicate verifies: {dup_outcome:?}"
            );
            audit_ids.push(audit_id);
        }
        // THE UNKNOWN BATCH INDEX.
        let error =
            crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, 99, 73).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-ledger-v2/batch-not-in-plan"
                }
            ),
            "the unknown batch index refuses: {error:?}"
        );
        // THE NON-DESTRUCTION: only the plan + the audits added;
        // every retirement record and object intact.
        let summary = medium.validate().unwrap();
        assert_eq!(
            summary.records,
            records_before + 1 + audit_ids.len() as u64,
            "only the plan and audit revisions were added"
        );
        assert!(medium.object_path(&r1).is_file());
        assert!(medium.object_path(&r2).is_file());
        // THE RESTART DISCOVERY.
        drop(medium);
        let mut reopened = LocalFileMedium::open(root.clone()).unwrap();
        for (batch, audit_id) in plan.batches.iter().map(|b| b.index).zip(&audit_ids) {
            let (found_id, _ledger) =
                crate::reclaim::find_installed_v2_audit(&mut reopened, &mk, batch)
                    .unwrap()
                    .unwrap();
            assert_eq!(found_id, *audit_id);
        }
        assert_eq!(
            crate::reclaim::find_installed_plan(&mut reopened, &mk)
                .unwrap()
                .unwrap()
                .0,
            plan_id
        );
        let _ = std::fs::remove_dir_all(&root);
        // THE MISSING EVIDENCE: a digest member deleted → the
        // hash-verified read refuses (never a fabricated manifest).
        {
            let (root, journal_root, mut medium, mk, r1, _r2, _shared, _private) =
                v2_plan_campaign("v2w-audits-missing");
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 74)
                .unwrap();
            std::fs::remove_file(medium.object_path(&r1)).unwrap();
            let error =
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, 0, 75).unwrap_err();
            assert!(
                matches!(error, MediumError::MissingObject { .. }),
                "the missing evidence refuses: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // THE CORRUPT OBJECT: a unit object's bytes corrupted → the
        // hash-verified read refuses.
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _shared, private) =
                v2_plan_campaign("v2w-audits-corrupt");
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 76)
                .unwrap();
            std::fs::write(medium.object_path(&private), b"corrupted-bytes").unwrap();
            let error =
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, 0, 77).unwrap_err();
            assert!(
                matches!(error, MediumError::HashMismatch { .. }),
                "the corrupt object refuses: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_v2_plan_publication_uncertainty_reconciliation() {
        // (V2-W §9 C/E) The plan publication's Unknown-outcome
        // discipline over a test-only wrapper (the established
        // single-shot injection pattern): Unknown->Installed
        // reconciles to ReconciledInstalled with the verified
        // identity, key, and exact canonical content; Unknown->
        // NotInstalled retries through the correct admission;
        // Unknown->Unverifiable refuses typed (never a false durable
        // confirmation).
        #[derive(Clone, Copy)]
        enum V2Inject {
            Installed,
            NotInstalled,
            Unverifiable,
        }
        struct V2UnknownMedium<'a> {
            inner: &'a mut LocalFileMedium,
            inject: V2Inject,
            armed: bool,
        }
        impl CanonicalMedium for V2UnknownMedium<'_> {
            fn head(&mut self) -> Result<Option<Hash>, MediumError> {
                self.inner.head()
            }
            fn append_admitted(
                &mut self,
                admitted: crate::medium::AdmittedAppend,
                expected_parent: Option<Hash>,
            ) -> Result<AppendOutcome, MediumError> {
                if !self.armed {
                    return self.inner.append_admitted(admitted, expected_parent);
                }
                self.armed = false;
                match self.inject {
                    V2Inject::Installed => {
                        self.inner.append_admitted(admitted, expected_parent)?;
                        Ok(AppendOutcome::Unknown {
                            reason: crate::medium::UnknownReason::CommitUncertain,
                        })
                    }
                    V2Inject::NotInstalled => Ok(AppendOutcome::Unknown {
                        reason: crate::medium::UnknownReason::ObjectWriteFailed,
                    }),
                    V2Inject::Unverifiable => Ok(AppendOutcome::Unknown {
                        reason: crate::medium::UnknownReason::HeadPublishFailed,
                    }),
                }
            }
            fn reconcile_unknown(
                &mut self,
                candidate: &Hash,
            ) -> Result<crate::medium::Reconciliation, MediumError> {
                match self.inject {
                    V2Inject::Installed => self.inner.reconcile_unknown(candidate),
                    V2Inject::NotInstalled => Ok(crate::medium::Reconciliation::NotInstalled {
                        head: self.inner.head()?,
                    }),
                    V2Inject::Unverifiable => Ok(crate::medium::Reconciliation::Unverifiable {
                        reason: "the injected uncertain outcome".to_owned(),
                    }),
                }
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
            fn begin_reclamation(
                &mut self,
                audit: &Hash,
                mk: &Hash,
                tick: u64,
            ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
                self.inner.begin_reclamation(audit, mk, tick)
            }
            fn verify_begin_authority(
                &mut self,
                audit: &Hash,
                mk: &Hash,
            ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
                self.inner.verify_begin_authority(audit, mk)
            }
        }
        for inject in [
            V2Inject::Installed,
            V2Inject::NotInstalled,
            V2Inject::Unverifiable,
        ] {
            let (root, journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
                v2_plan_campaign("v2w-uncertain");
            let mut wrapper = V2UnknownMedium {
                inner: &mut medium,
                inject,
                armed: true,
            };
            let result = crate::reclaim::publish_reclamation_plan(
                &mut wrapper,
                &mk,
                1024,
                &journal_root,
                80,
            );
            match inject {
                V2Inject::Installed => {
                    let (id, outcome) = result.unwrap();
                    assert!(
                        matches!(
                            outcome,
                            crate::reclaim::PlanPublicationOutcome::ReconciledInstalled {
                                id: reconciled,
                            } if reconciled == id
                        ),
                        "the installed unknown reconciles: {outcome:?}"
                    );
                }
                V2Inject::NotInstalled => {
                    let (_id, outcome) = result.unwrap();
                    assert!(
                        matches!(
                            outcome,
                            crate::reclaim::PlanPublicationOutcome::Accepted { .. }
                        ),
                        "the not-installed unknown retries through the admission: {outcome:?}"
                    );
                }
                V2Inject::Unverifiable => {
                    let error = result.unwrap_err();
                    assert!(
                        matches!(
                            error,
                            MediumError::Chain {
                                rule: "reclamation-plan/unknown-unverifiable"
                            }
                        ),
                        "the unverifiable unknown refuses: {error:?}"
                    );
                }
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_v2_f4_journal_authority_matrix() {
        // (V2-W F4) The campaign-bound journal authority, end to end:
        // the CORRECT campaign's genuinely rotated nonempty lineage
        // publishes the plan with the EXACT authenticated obligations
        // (the F4 goal — the interim boundary superseded by the
        // bound lifecycle); a FOREIGN campaign's root (bound or
        // empty) refuses at the location check; a MISSING journal
        // after the established binding refuses as missing
        // authoritative evidence (never the empty fallback); the
        // genuinely empty obligation set publishes; the same-state
        // retry verifies as the exact duplicate; and the
        // contradictory evidence (a further rotation) refuses as
        // the conflicting content — the installed plan's
        // obligations are immutable.
        let (root, journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
            v2_plan_campaign("v2w-f4-authority");
        // THE CORRECT NONEMPTY ROTATION: the genuine bound rotation
        // of THIS campaign's root — the plan publishes with the
        // EXACT stale-generation obligations.
        let bound_founding = crate::reclaim::founding_identity_medium(&mut medium).unwrap();
        let bound_root_id = crate::reclaim::journal_root_id(
            &bound_founding,
            &crate::reclaim::JournalRootBinding::normalize_location(&journal_root),
        );
        crate::rotation::rotate_journal_with(
            &journal_root,
            &[],
            &[],
            &[],
            Some((&bound_founding, &bound_root_id)),
        )
        .unwrap();
        let (plan_id, outcome) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 90)
                .unwrap();
        assert!(
            matches!(
                outcome,
                crate::reclaim::PlanPublicationOutcome::Accepted { .. }
            ),
            "the bound rotated campaign publishes: {outcome:?}"
        );
        let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
            .unwrap()
            .unwrap();
        assert!(
            !plan.journal_files.is_empty(),
            "the rotation's stale generations bind as the plan's obligations"
        );
        // THE SAME-STATE RETRY: the exact duplicate.
        let (dup_id, dup_outcome) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 91)
                .unwrap();
        assert!(
            matches!(
                dup_outcome,
                crate::reclaim::PlanPublicationOutcome::DuplicateVerified { id } if id == plan_id
            ),
            "the same-state retry verifies as the exact duplicate: {dup_outcome:?}"
        );
        assert_eq!(dup_id, plan_id);
        // THE CONTRADICTORY EVIDENCE (a further rotation): the
        // different obligations under the same key refuse — the
        // installed plan's obligations are immutable.
        let bound_founding = crate::reclaim::founding_identity_medium(&mut medium).unwrap();
        let bound_root_id = crate::reclaim::journal_root_id(
            &bound_founding,
            &crate::reclaim::JournalRootBinding::normalize_location(&journal_root),
        );
        crate::rotation::rotate_journal_with(
            &journal_root,
            &[],
            &[],
            &[],
            Some((&bound_founding, &bound_root_id)),
        )
        .unwrap();
        let error =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 92)
                .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-plan/duplicate-content-mismatch"
                }
            ),
            "the contradictory journal evidence refuses: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        // THE FOREIGN ROOT (a bound root of a DIFFERENT campaign):
        // the location check refuses — campaign B's lineage is
        // never campaign A's authority.
        {
            let (root_b, journal_root_b, mut medium_b, mk_b, _r1, _r2, _s, _p) =
                v2_plan_campaign("v2w-f4-foreign");
            let error = crate::reclaim::publish_reclamation_plan(
                &mut medium_b,
                &mk_b,
                1024,
                &journal_root,
                93,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "journal-root/location-mismatch"
                    }
                ),
                "the foreign campaign's root refuses at the location check: {error:?}"
            );
            assert!(
                crate::reclaim::find_installed_plan(&mut medium_b, &mk_b)
                    .unwrap()
                    .is_none()
            );
            let _ = std::fs::remove_dir_all(&root_b);
            let _ = std::fs::remove_dir_all(&journal_root_b);
        }
        // THE MISSING JOURNAL (the binding established, the journal
        // absent): missing authoritative evidence — fail closed,
        // never the empty fallback.
        {
            let (root_m, journal_root_m, mut medium_m, mk_m, _r1, _r2, _s, _p) =
                v2_plan_campaign("v2w-f4-missing");
            std::fs::remove_file(&journal_root_m).unwrap();
            let error = crate::reclaim::publish_reclamation_plan(
                &mut medium_m,
                &mk_m,
                1024,
                &journal_root_m,
                94,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "journal-root/active-identity"
                    }
                ),
                "the missing bound journal refuses as missing evidence: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root_m);
        }
        // THE GENUINELY EMPTY SET (the bound journal, no rotation):
        // publishes with the empty obligations.
        {
            let (root_e, journal_root_e, mut medium_e, mk_e, _r1, _r2, _s, _p) =
                v2_plan_campaign("v2w-f4-empty");
            let (_id, outcome) = crate::reclaim::publish_reclamation_plan(
                &mut medium_e,
                &mk_e,
                1024,
                &journal_root_e,
                95,
            )
            .unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::PlanPublicationOutcome::Accepted { .. }
                ),
                "the bound empty journal legitimately publishes: {outcome:?}"
            );
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium_e, &mk_e)
                .unwrap()
                .unwrap();
            assert!(plan.journal_files.is_empty());
            let _ = std::fs::remove_dir_all(&root_e);
        }
    }

    #[test]
    fn the_v2_f4_binding_admission_and_bootstrap_matrix() {
        // (V2-W F4) The binding admission and the bootstrap's crash
        // states: the unbound campaign's plan publication refuses
        // (journal-root/not-bound); the exact-duplicate binding
        // verifies; the CONFLICTING binding (a competing root)
        // refuses — never a rebind; the generic append refuses the
        // binding domain; and the legacy journal at a bound
        // campaign's root refuses the identity check (no
        // reinterpretation).
        let (root, _journal_root, mut medium, _mk, _r1, _r2, _shared, _private) =
            v2_plan_campaign("v2w-f4-binding");
        // THE EXACT DUPLICATE.
        let founding = crate::reclaim::founding_identity_medium(&mut medium).unwrap();
        let installed = crate::reclaim::find_installed_journal_root_binding(&mut medium, &founding)
            .unwrap()
            .unwrap();
        let (_dup_id, dup_outcome) = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding,
            &installed.1.root_id,
            &installed.1.location,
            96,
        )
        .unwrap();
        assert!(
            matches!(
                dup_outcome,
                crate::reclaim::JournalRootBindingOutcome::DuplicateVerified { .. }
            ),
            "the exact binding verifies: {dup_outcome:?}"
        );
        // THE CONFLICTING BINDING (the competing root ID + location —
        // a REAL non-zero identity derived from a different
        // location: the strict decoder refuses zero-identity fields
        // before the conflict check would run). The competing
        // location is the CANONICAL normalized form of a genuinely
        // different absolute path (platform-portable: a bare
        // "/competing/root" is not an absolute descriptor on
        // Windows, and the F4-R13 canonical check would refuse it
        // before the conflict discipline — the competition must
        // reach the CONFLICT, not the descriptor admission).
        let competing_location = crate::reclaim::JournalRootBinding::normalize_location(
            std::path::Path::new("/competing/root"),
        );
        let competing_root_id = crate::reclaim::journal_root_id(&founding, &competing_location);
        let error = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding,
            &competing_root_id,
            &competing_location,
            97,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "journal-root/binding-conflict"
                }
            ),
            "the competing root refuses — never a rebind: {error:?}"
        );
        // THE GENERIC-APPEND BYPASS.
        let binding_object = StoredObject::new(
            crate::reclaim::JournalRootBinding {
                founding,
                root_id: installed.1.root_id,
                location: "/bypass/attempt".to_owned(),
            }
            .canonical_bytes(),
        )
        .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record =
            RevisionRecord::regular(head, epoch, 98).with_content_refs(vec![binding_object.hash()]);
        let error = medium
            .append(&record, std::slice::from_ref(&binding_object), Some(head))
            .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "journal-root/generic-append-refused"
                }
            ),
            "the generic append refuses the binding domain: {error:?}"
        );
        // THE UNBOUND CAMPAIGN (no binding): the plan publication
        // refuses — v2 trusted publication requires the bound
        // lifecycle.
        {
            // A fresh unbound chain (the p2r1 campaign shape — a
            // checkpoint with no binding): the refusal fires at the
            // not-bound check BEFORE any journal-root evaluation.
            let (root_p, mut medium_p, _audit_p, mk_p, _directive_p, _a, _b, _c, _d, _e, _f) =
                p2r1_prefix_campaign("v2w-f4-unbound-p2r1", false);
            let unbound_root = root_p.join("any-journal-root");
            let error = crate::reclaim::publish_reclamation_plan(
                &mut medium_p,
                &mk_p,
                1024,
                &unbound_root,
                99,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "journal-root/not-bound"
                    }
                ),
                "the unbound campaign refuses v2 publication: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root_p);
        }
        // THE LEGACY JOURNAL at the bound root (the
        // no-reinterpretation rule).
        {
            let (root_l, journal_root_l, mut medium_l, mk_l, _r1, _r2, _s, _p) =
                v2_plan_campaign("v2w-f4-legacy-mix");
            std::fs::remove_file(&journal_root_l).unwrap();
            // The legacy journal is created and its handle RELEASED
            // (the sidecar lock frees) before the publication takes
            // the root's lock — the same-process flock discipline.
            drop(crate::journal::T1Journal::open(&journal_root_l).unwrap());
            let error = crate::reclaim::publish_reclamation_plan(
                &mut medium_l,
                &mk_l,
                1024,
                &journal_root_l,
                100,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "journal-root/active-identity"
                    }
                ),
                "the legacy journal at the bound root refuses (no reinterpretation): {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root_l);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_f1_oversized_unit_after_split_refuses_before_publication() {
        // (V2W-F1, restored by F4-R4) The singleton check on BOTH
        // sides of the split: the small-then-oversized fixture —
        // the first unit fits, the combined proposal overflows, the
        // second unit alone overflows — refuses BEFORE publication.
        // The positive control: the multi-batch partition with
        // consecutive indices and the complete range exactly once.
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _s, _p) =
                v2_plan_campaign_variant("v2wf1r-oversized", false, false, false, 100);
            let (_id, outcome) = crate::reclaim::publish_reclamation_plan(
                &mut medium,
                &mk,
                crate::reclaim::MAX_RECLAMATION_LEDGER_BYTES,
                &journal_root,
                50,
            )
            .unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::PlanPublicationOutcome::Accepted { .. }
                ),
                "the generous limit admits: {outcome:?}"
            );
            let (root2, journal_root2, mut medium2, mk2, _a, _b, _c, _d) =
                v2_plan_campaign_variant("v2wf1r-oversized-2", false, false, false, 100);
            let error = crate::reclaim::publish_reclamation_plan(
                &mut medium2,
                &mk2,
                1024,
                &journal_root2,
                51,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-plan/unit-exceeds-limit"
                    }
                ),
                "the oversized unit after the split refuses: {error:?}"
            );
            assert!(
                crate::reclaim::find_installed_plan(&mut medium2, &mk2)
                    .unwrap()
                    .is_none()
            );
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&root2);
        }
        {
            let (root, journal_root, mut medium, mk, r1, r2, _s, _p) =
                v2_plan_campaign_variant("v2wf1r-multibatch", false, false, false, 0);
            let (_id, outcome) =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 400, &journal_root, 52)
                    .unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::PlanPublicationOutcome::Accepted { .. }
                ),
                "the multi-batch plan publishes: {outcome:?}"
            );
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
                .unwrap()
                .unwrap();
            assert!(plan.batches.len() >= 2);
            for (position, batch) in plan.batches.iter().enumerate() {
                assert_eq!(batch.index, position as u64);
                assert!(!batch.records.is_empty());
            }
            let mut covered: Vec<Hash> = plan
                .batches
                .iter()
                .flat_map(|batch| batch.records.iter().copied())
                .collect();
            covered.sort();
            let mut expected = vec![r1, r2];
            let mk_record = medium.read_record(&mk).unwrap();
            let meta_bytes = medium.read_object(&mk_record.content_refs[0]).unwrap();
            let metadata = CheckpointMetadata::decode(&meta_bytes).unwrap();
            expected.push(metadata.sk);
            expected.sort();
            assert_eq!(covered, expected, "the complete range exactly once");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_v2_f2_fabricated_checkpoint_evidence_refuses_plan_publication() {
        // (V2W-F2, restored by F4-R4) The plan admission's complete
        // first-execution evidence: the fabricated digest, the wrong
        // founding seed, and the incoherent epoch claims each refuse
        // BEFORE any plan installs; the genuine evidence produces
        // the plan with the original accepted bindings.
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _s, _p) =
                v2_plan_campaign_variant("v2wf2r-digest", true, false, false, 0);
            let error =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 60)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/range-digest-mismatch"
                    }
                ),
                "the fabricated digest refuses: {error:?}"
            );
            assert!(
                crate::reclaim::find_installed_plan(&mut medium, &mk)
                    .unwrap()
                    .is_none()
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _s, _p) =
                v2_plan_campaign_variant("v2wf2r-seed", false, true, false, 0);
            let error =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 61)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclaim/seed-not-founding-derived"
                    }
                ),
                "the wrong founding seed refuses: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _s, _p) =
                v2_plan_campaign_variant("v2wf2r-order", false, false, true, 0);
            let error =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 62)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/range-digest-mismatch"
                    }
                ),
                "the incoherent epoch claims refuse: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        {
            let (root, journal_root, mut medium, mk, r1, r2, _s, _p) =
                v2_plan_campaign_variant("v2wf2r-genuine", false, false, false, 0);
            let (_id, outcome) =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 63)
                    .unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::PlanPublicationOutcome::Accepted { .. }
                ),
                "the genuine evidence publishes: {outcome:?}"
            );
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
                .unwrap()
                .unwrap();
            let members: Vec<Hash> = plan
                .batches
                .iter()
                .flat_map(|b| b.records.iter().copied())
                .collect();
            assert!(members.contains(&r1) && members.contains(&r2));
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_v2_f3_second_reference_integrity_refuses_both_admissions() {
        // (V2W-F3, restored by F4-R4) Every direct content reference
        // verified at both writer boundaries: the corrupt or removed
        // SECOND reference refuses the plan admission (the evidence
        // core) and the batch-audit admission (the collector).
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _s, private) =
                v2_plan_campaign_variant("v2wf3r-plan-corrupt", false, false, false, 0);
            std::fs::write(medium.object_path(&private), b"corrupted-second-ref").unwrap();
            let error =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 70)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::HashMismatch { .. } | MediumError::MissingObject { .. }
                ),
                "the corrupt second reference refuses the plan admission: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        {
            let (root, journal_root, mut medium, mk, _r1, _r2, _s, private) =
                v2_plan_campaign_variant("v2wf3r-audit-corrupt", false, false, false, 0);
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &journal_root, 72)
                .unwrap();
            std::fs::write(medium.object_path(&private), b"corrupted-second-ref").unwrap();
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
                .unwrap()
                .unwrap();
            for batch in plan.batches.iter().map(|b| b.index) {
                let error =
                    crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, batch, 73)
                        .unwrap_err();
                assert!(
                    matches!(
                        error,
                        MediumError::HashMismatch { .. } | MediumError::MissingObject { .. }
                    ),
                    "the corrupt second reference refuses the audit admission: {error:?}"
                );
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_v2_f5b_key_mismatched_installed_plan_never_authorizes_audits() {
        // (V2W-F5B, restored by F4-R4) The installed-plan identity
        // binding: a record with the expected deterministic key but
        // decoded content naming a different MK never authorizes
        // batch publication.
        let (root, _journal_root, mut medium, mk, r1, r2, _s, _p) =
            v2_plan_campaign_variant("v2wf5br", false, false, false, 0);
        let foreign = crate::reclaim::ReclaimPlan {
            mk: r1,
            sk: r2,
            range_digest: [9u8; 32],
            seed: [9u8; 32],
            ledger_byte_limit: 1024,
            batches: vec![crate::reclaim::PlanBatch {
                index: 0,
                records: vec![r1],
            }],
            journal_files: Vec::new(),
        };
        let object = StoredObject::new(foreign.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record = RevisionRecord::regular(head, epoch, 80)
            .with_idempotency_key(crate::reclaim::reclamation_plan_idempotency_key(&mk))
            .with_content_refs(vec![object.hash()]);
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
        medium.append_admitted(admitted, Some(head)).unwrap();
        let error =
            crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, 0, 81).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-ledger-v2/plan-key-mismatch"
                }
            ),
            "the key-mismatched installed plan never authorizes: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_f4_direct_first_binding_requires_the_fresh_bootstrap_shape() {
        // (F4-R6) The trusted-bootstrap restriction on the FIRST
        // binding: an existing campaign with real history (a legacy
        // lineage, no binding installed) refuses the direct
        // first-binding publication typed — an ordinary external
        // caller cannot establish an arbitrary binding over an
        // established campaign's fate — while the fresh-bootstrap
        // shape (the campaign carrying exactly its founding) still
        // publishes through the trusted admission, and a bound
        // campaign's later duplicate verification is unaffected
        // (the occupied key routes it away from this gate).
        {
            // THE HISTORY CASE: the p2r1 campaign (a full checkpoint
            // lineage, unbound) — the head is far past the founding.
            let (root, mut medium, _audit, _mk, _directive, _sk, _r3, _r2, _r1, _shared, _private) =
                p2r1_prefix_campaign("v2w-f4-r6-history", false);
            let founding = crate::reclaim::founding_identity_medium(&mut medium).unwrap();
            let journal_root = std::env::temp_dir().join("s7-v2w-f4-r6-history-root");
            let location = crate::reclaim::JournalRootBinding::normalize_location(&journal_root);
            let root_id = crate::reclaim::journal_root_id(&founding, &location);
            let error = crate::reclaim::publish_journal_root_binding(
                &mut medium,
                &founding,
                &root_id,
                &location,
                90,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "journal-root/not-fresh-bootstrap"
                    }
                ),
                "the direct first binding over existing history refuses: {error:?}"
            );
            assert!(
                crate::reclaim::find_installed_journal_root_binding(&mut medium, &founding)
                    .unwrap()
                    .is_none(),
                "no binding was installed"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        {
            // THE FRESH SHAPE: the campaign carrying exactly its
            // founding (the bootstrap's own intermediate state) —
            // the direct publication admits (the crossing's
            // open_bound_bootstrap republishes through exactly this
            // gate on the crash-between re-entry).
            let root = temp_store("v2w-f4-r6-fresh");
            let mut medium = LocalFileMedium::open(root.clone()).unwrap();
            let founding =
                RevisionRecord::founding(AuthorityMode::Standalone, "v2w-f4-r6-fresh", 0);
            medium.append(&founding, &[], None).unwrap();
            let journal_root = std::env::temp_dir()
                .join(format!("s7-v2w-f4-r6-fresh-journal-{}", std::process::id()));
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
            let (_id, outcome) = crate::reclaim::publish_journal_root_binding(
                &mut medium,
                &founding_id,
                &root_id,
                &location,
                1,
            )
            .unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::JournalRootBindingOutcome::Accepted { .. }
                ),
                "the fresh-shape first binding publishes: {outcome:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&journal_root);
        }
    }

    #[test]
    fn the_v2_f4_first_binding_requires_durable_bootstrap_evidence() {
        // (F4-R8) The first-binding admission's bootstrap-evidence
        // verification: an arbitrary external caller holding a
        // founding hash, a root ID, and a location can NEVER
        // establish the first binding — the append demands the root
        // ID's independent derivation from the founding and the
        // normalized location, and the DURABLE journal-bootstrap
        // evidence (the bound journal file existing at the claimed
        // location with the exact expected identities). A missing
        // journal refuses; a wrong derived root ID refuses; a
        // legacy journal at the location refuses; the genuine
        // bootstrap evidence publishes (the crossing's
        // journal-first ordering always leaves the residue).
        let root = temp_store("v2w-f4-r8");
        let mut medium = LocalFileMedium::open(root.clone()).unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "v2w-f4-r8", 0);
        medium.append(&founding, &[], None).unwrap();
        let founding_id = founding.id();
        let journal_root =
            std::env::temp_dir().join(format!("s7-v2w-f4-r8-journal-{}", std::process::id()));
        let location = crate::reclaim::JournalRootBinding::normalize_location(&journal_root);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        // (a) THE MISSING JOURNAL: the values alone (the founding
        // hash, the correctly derived root ID, the location) are
        // never authorization — no durable bootstrap evidence
        // exists at the location.
        let error = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            90,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "journal-root/bootstrap-evidence-missing"
                }
            ),
            "the missing durable bootstrap evidence refuses: {error:?}"
        );
        assert!(
            crate::reclaim::find_installed_journal_root_binding(&mut medium, &founding_id)
                .unwrap()
                .is_none(),
            "no binding was installed"
        );
        // (b) THE WRONG DERIVED ROOT ID: a caller-supplied root ID
        // that is not the deterministic derivation from the founding
        // and the normalized location refuses.
        let wrong_root_id = crate::reclaim::journal_root_id(&founding_id, "/wrong/location");
        let error = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &wrong_root_id,
            &location,
            91,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "journal-root/root-id-derivation"
                }
            ),
            "the wrong derived root ID refuses: {error:?}"
        );
        // (c) THE LEGACY JOURNAL AT THE LOCATION: a file carrying a
        // legacy header is not the expected bound journal — never
        // reinterpreted as bootstrap evidence.
        drop(crate::journal::T1Journal::open(&journal_root).unwrap());
        let error = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            92,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "journal-root/bootstrap-evidence-invalid"
                }
            ),
            "the legacy journal at the location refuses: {error:?}"
        );
        assert!(
            crate::reclaim::find_installed_journal_root_binding(&mut medium, &founding_id)
                .unwrap()
                .is_none(),
            "still no binding was installed"
        );
        let _ = std::fs::remove_file(&journal_root);
        let _ = std::fs::remove_file(journal_root.with_extension("lock"));
        // (d) THE GENUINE EVIDENCE: the bound journal durably created
        // at the location with the exact expected identities (the
        // trusted bootstrap's own durable step) publishes.
        drop(
            crate::reclaim::create_campaign_bound_journal(&mut medium, &journal_root, &founding_id)
                .unwrap(),
        );
        let (_id, outcome) = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            93,
        )
        .unwrap();
        assert!(
            matches!(
                outcome,
                crate::reclaim::JournalRootBindingOutcome::Accepted { .. }
            ),
            "the genuine durable bootstrap evidence publishes: {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&journal_root);
        let _ = std::fs::remove_file(journal_root.with_extension("lock"));
    }

    #[test]
    fn the_v2_f4_binding_location_must_be_the_canonical_descriptor() {
        // (F4-R13) The canonical root-location descriptor: the
        // binding admission's supplied location must BE the exact
        // normalized absolute form required by the accepted contract
        // — a relative descriptor (even with a matching journal at
        // the real relative path and a root ID derived from the
        // unnormalized string) refuses BEFORE any publication work,
        // so the permanent key is never installed over a descriptor
        // the verifications could not round-trip; the properly
        // normalized absolute equivalent publishes.
        let root = temp_store("v2w-f4-r13");
        let mut medium = LocalFileMedium::open(root.clone()).unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "v2w-f4-r13", 0);
        medium.append(&founding, &[], None).unwrap();
        let founding_id = founding.id();
        // A REAL relative path (under the test's working directory)
        // carrying a matching bound journal, with the root ID
        // derived from the UNNORMALIZED string.
        let relative = std::path::Path::new("s7-r13-relative/journal.t1");
        let relative_text = relative.to_str().unwrap().to_owned();
        let wrong_root_id = crate::reclaim::journal_root_id(&founding_id, &relative_text);
        {
            drop(
                crate::reclaim::create_campaign_bound_journal(&mut medium, relative, &founding_id)
                    .unwrap(),
            );
        }
        assert!(relative.exists(), "the relative-path journal is real");
        // THE DIRECT PUBLICATION with the relative descriptor and
        // its unnormalized derivation refuses — the canonical check
        // fires before any publication work.
        let error = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &wrong_root_id,
            &relative_text,
            94,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "journal-root/location-not-canonical"
                }
            ),
            "the relative location descriptor refuses: {error:?}"
        );
        assert!(
            crate::reclaim::find_installed_journal_root_binding(&mut medium, &founding_id)
                .unwrap()
                .is_none(),
            "the permanent key was never installed"
        );
        let _ = std::fs::remove_file(relative);
        let _ = std::fs::remove_dir_all("s7-r13-relative");
        // THE PROPERLY NORMALIZED ABSOLUTE EQUIVALENT publishes (the
        // trusted bootstrap's own descriptor form).
        let absolute =
            std::env::temp_dir().join(format!("s7-v2w-f4-r13-journal-{}", std::process::id()));
        let location = crate::reclaim::JournalRootBinding::normalize_location(&absolute);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        {
            drop(
                crate::reclaim::create_campaign_bound_journal(&mut medium, &absolute, &founding_id)
                    .unwrap(),
            );
        }
        let (_id, outcome) = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            95,
        )
        .unwrap();
        assert!(
            matches!(
                outcome,
                crate::reclaim::JournalRootBindingOutcome::Accepted { .. }
            ),
            "the canonical absolute descriptor publishes: {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&absolute);
        let _ = std::fs::remove_file(absolute.with_extension("lock"));
    }

    // -------------------------------------------------------------
    // (V2X-CP1) THE TRUSTED ALL-AUDITS READER
    // -------------------------------------------------------------

    /// The complete publication helper: the plan at the given limit
    /// plus EVERY plan-listed batch audit (the all-audits-first
    /// discipline), returning (plan_id, plan, audit_ids).
    fn v2x_publish_all(
        medium: &mut LocalFileMedium,
        mk: &Hash,
        journal_root: &std::path::Path,
        limit: u64,
    ) -> (Hash, crate::reclaim::ReclaimPlan, Vec<Hash>) {
        let (plan_id, _) =
            crate::reclaim::publish_reclamation_plan(medium, mk, limit, journal_root, 100).unwrap();
        let (_, plan) = crate::reclaim::find_installed_plan(medium, mk)
            .unwrap()
            .unwrap();
        let mut audit_ids = Vec::new();
        for batch in plan.batches.iter().map(|b| b.index) {
            let (audit_id, outcome) =
                crate::reclaim::publish_reclamation_ledger_v2(medium, mk, batch, 101).unwrap();
            assert!(
                matches!(
                    outcome,
                    crate::reclaim::LedgerV2PublicationOutcome::Accepted { id } if id == audit_id
                ),
                "the batch audit publishes: {outcome:?}"
            );
            audit_ids.push(audit_id);
        }
        (plan_id, plan, audit_ids)
    }

    /// The in-crate adversarial install (the F5B pattern): a record
    /// under the given deterministic key carrying the given content
    /// object — the route around the publication gates the reader's
    /// own checks must independently refuse.
    fn v2x_install_under_key(
        medium: &mut LocalFileMedium,
        key: Vec<u8>,
        object: &StoredObject,
        tick: u64,
    ) -> Hash {
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record = RevisionRecord::regular(head, epoch, tick)
            .with_idempotency_key(key)
            .with_content_refs(vec![object.hash()]);
        let id = record.id();
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(object));
        medium.append_admitted(admitted, Some(head)).unwrap();
        id
    }

    #[test]
    fn the_v2_reader_certifies_the_complete_first_execution_ready_state() {
        // (V2X-CP1) THE TRUSTED POSITIVE: the complete plan + every
        // plan-listed audit, over the multi-batch split (the shared
        // object crossing unit and batch boundaries) — the reader
        // certifies FIRST-EXECUTION-READY with the exact revision
        // identities, STRICTLY READ-ONLY (nothing published, nothing
        // deleted), and the restart (a fresh handle) certifies
        // identically.
        let (root, journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
            v2_plan_campaign("v2x-cp1-ready");
        let records_before = medium.validate().unwrap().records;
        let (plan_id, plan, audit_ids) = v2x_publish_all(&mut medium, &mk, &journal_root, 400);
        assert!(
            plan.batches.len() >= 2,
            "the fixture splits into multiple batches (the shared object crosses them)"
        );
        match crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap() {
            crate::reclaim::V2ExecutionReadiness::ExecutionReady {
                plan_record,
                plan_object,
                audit_records,
            } => {
                assert_eq!(plan_record, plan_id);
                // The plan's RECORD identity is distinct from the
                // payload object's hash (the recorded D1 obligation:
                // any future begin binds the record identity).
                let content_ref = medium.read_record(&plan_id).unwrap().content_refs[0];
                assert_eq!(plan_object, content_ref);
                assert_ne!(plan_record, plan_object);
                assert_eq!(audit_records, audit_ids);
            }
            other => panic!("the complete state certifies ready: {other:?}"),
        }
        // STRICTLY READ-ONLY: exactly the plan + the audits were
        // added by the publication; the reader itself added nothing.
        assert_eq!(
            medium.validate().unwrap().records,
            records_before + 1 + audit_ids.len() as u64
        );
        // THE RESTART: a fresh handle certifies identically.
        drop(medium);
        let mut reopened = LocalFileMedium::open(root.clone()).unwrap();
        match crate::reclaim::verify_v2_execution_readiness(&mut reopened, &mk).unwrap() {
            crate::reclaim::V2ExecutionReadiness::ExecutionReady { plan_record, .. } => {
                assert_eq!(plan_record, plan_id);
            }
            other => panic!("the restart certifies ready: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_reader_classifies_the_incomplete_publication_states() {
        // (V2X-CP1) §5: the legitimate non-destructive
        // mid-publication states are CLASSIFIED, never certified —
        // the genuine checkpoint with no plan, and the plan with a
        // partial audit set.
        let (root, journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
            v2_plan_campaign("v2x-cp1-incomplete");
        // (a) No plan: the pre-publication state.
        match crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap() {
            crate::reclaim::V2ExecutionReadiness::PublicationIncomplete { missing_batches } => {
                assert!(missing_batches.is_empty(), "no plan is published yet");
            }
            other => panic!("the pre-publication state classifies: {other:?}"),
        }
        // (b) The plan with only the FIRST batch's audit published.
        let (_plan_id, plan, _audit_ids) = {
            let (plan_id, _) =
                crate::reclaim::publish_reclamation_plan(&mut medium, &mk, 400, &journal_root, 100)
                    .unwrap();
            let (_, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk)
                .unwrap()
                .unwrap();
            let (_audit_id, _) =
                crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk, 0, 101).unwrap();
            (plan_id, plan, Vec::<Hash>::new())
        };
        match crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap() {
            crate::reclaim::V2ExecutionReadiness::PublicationIncomplete { missing_batches } => {
                let expected: Vec<u64> = plan
                    .batches
                    .iter()
                    .map(|b| b.index)
                    .filter(|index| *index != 0)
                    .collect();
                assert_eq!(missing_batches, expected);
            }
            other => panic!("the partial audit set classifies: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_reader_refuses_the_mixed_v1_v2_interpretation() {
        // (V2X-CP1) The reader-side W2 mirror: a v1 audit coexisting
        // with an installed plan (constructible only by bypassing
        // the publication gates — the in-crate adversarial install)
        // refuses typed. One interpretation per MK, ever.
        let (root, journal_root, mut medium, mk, r1, r2, _shared, _private) =
            v2_plan_campaign("v2x-cp1-mixed");
        let (_plan_id, _plan, _audit_ids) = v2x_publish_all(&mut medium, &mk, &journal_root, 400);
        let v1_shaped = crate::reclaim::ReclamationLedger {
            mk,
            sk: r2,
            range_digest: [0u8; 32],
            seed: [0u8; 32],
            records: 1,
            reclaimed_keys: 0,
            journal_files: Vec::new(),
            units: vec![crate::reclaim::ReclaimUnit {
                record: r1,
                objects: Vec::new(),
            }],
        };
        let object = StoredObject::new(v1_shaped.canonical_bytes()).unwrap();
        v2x_install_under_key(
            &mut medium,
            crate::reclaim::reclamation_ledger_idempotency_key(&mk),
            &object,
            110,
        );
        let error = crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "v2-readiness/v1-audit-mixed"
                }
            ),
            "the mixed interpretation refuses: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_reader_refuses_the_wrongly_bound_installed_evidence() {
        // (V2X-CP1) The binding refusals against adversarially
        // installed evidence (the in-crate route around the
        // publication gates): (a) a plan under the right
        // deterministic key whose content names foreign identities;
        // (b) a batch audit under the right (MK, batch) key whose
        // fields disagree with the plan.
        let (root, _journal_root, mut medium, mk, r1, r2, _shared, _private) =
            v2_plan_campaign("v2x-cp1-bound");
        // (a) The wrongly bound plan (the F5B shape, refused now by
        // the READER rather than the publication): the doctored form
        // occupies the plan key DIRECTLY on the fresh campaign — no
        // genuine plan is published (the keys are chain-unique).
        {
            let foreign = crate::reclaim::ReclaimPlan {
                mk: r1,
                sk: r2,
                range_digest: [9u8; 32],
                seed: [9u8; 32],
                ledger_byte_limit: 1024,
                batches: vec![crate::reclaim::PlanBatch {
                    index: 0,
                    records: vec![r1],
                }],
                journal_files: Vec::new(),
            };
            let object = StoredObject::new(foreign.canonical_bytes()).unwrap();
            v2x_install_under_key(
                &mut medium,
                crate::reclaim::reclamation_plan_idempotency_key(&mk),
                &object,
                120,
            );
            let error =
                crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "v2-readiness/plan-binding"
                    }
                ),
                "the foreign plan refuses: {error:?}"
            );
        }
        // (b) The wrongly bound audit: the genuine plan and the
        // audits EXCEPT batch 0 (whose key the doctored form
        // occupies — the keys are chain-unique), the doctored
        // ledger's SK, digest, and seed disagreeing with the plan.
        {
            let (root_b, journal_root_b, mut medium_b, mk_b, _r1b, r2b, _shared_b, _private_b) =
                v2_plan_campaign("v2x-cp1-bound-audit");
            let plan = v2x_publish_all_except(&mut medium_b, &mk_b, &journal_root_b, 400, 0);
            let doctored = crate::reclaim::ReclamationLedgerV2 {
                mk: mk_b,
                sk: r2b,
                batch: 0,
                range_digest: [9u8; 32],
                seed: [9u8; 32],
                records: 0,
                reclaimed_keys: 0,
                units: Vec::new(),
            };
            let object = StoredObject::new(doctored.canonical_bytes()).unwrap();
            v2x_install_under_key(
                &mut medium_b,
                crate::reclaim::reclamation_ledger_v2_idempotency_key(&mk_b, 0),
                &object,
                121,
            );
            let error =
                crate::reclaim::verify_v2_execution_readiness(&mut medium_b, &mk_b).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "v2-readiness/audit-binding"
                    }
                ),
                "the doctored audit refuses: {error:?}"
            );
            let _ = plan;
            let _ = std::fs::remove_dir_all(&root_b);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The plan plus every plan-listed batch audit EXCEPT the given
    /// batch index (whose deterministic key stays unoccupied for an
    /// adversarial in-crate install) — the keys are chain-unique, so
    /// each doctored form needs its own fresh campaign.
    fn v2x_publish_all_except(
        medium: &mut LocalFileMedium,
        mk: &Hash,
        journal_root: &std::path::Path,
        limit: u64,
        skip: u64,
    ) -> crate::reclaim::ReclaimPlan {
        let (_plan_id, _) =
            crate::reclaim::publish_reclamation_plan(medium, mk, limit, journal_root, 100).unwrap();
        let (_, plan) = crate::reclaim::find_installed_plan(medium, mk)
            .unwrap()
            .unwrap();
        for batch in plan.batches.iter().map(|b| b.index) {
            if batch == skip {
                continue;
            }
            crate::reclaim::publish_reclamation_ledger_v2(medium, mk, batch, 101).unwrap();
        }
        plan
    }

    #[test]
    fn the_v2_reader_refuses_the_membership_and_order_violations() {
        // (V2X-CP1) The ordered-unit refusals: the adversarially
        // installed batch-0 audits (under batch 0's own
        // plan-listed key — the genuine publication skipped) carrying
        // (a) an OMITTED unit, (b) an INJECTED foreign unit (a
        // record from a LATER batch — not a batch-0 member), (c) the
        // correct set in the WRONG order — set equality is never
        // sufficient; the deterministic order is the identity, and
        // the reader reaches the ORDER check (the completeness and
        // binding checks pass first). The limit 460 splits the
        // fixture's range so batch 0 carries TWO records (the
        // reorder is meaningful; a one-record batch reversed is
        // itself).
        for (label, doctor) in [("omitted", 1u8), ("injected", 0), ("reordered", 2)] {
            let (root, journal_root, mut medium, mk, r1, _r2, _shared, _private) =
                v2_plan_campaign(&format!("v2x-cp1-order-{label}"));
            let plan = v2x_publish_all_except(&mut medium, &mk, &journal_root, 460, 0);
            let batch = &plan.batches[0];
            assert!(
                batch.records.len() >= 2,
                "the limit splits a multi-record batch 0 (the reorder is meaningful)"
            );
            let genuine: Vec<crate::reclaim::ReclaimUnit> = batch
                .records
                .iter()
                .map(|record| crate::reclaim::ReclaimUnit {
                    record: *record,
                    objects: crate::reclaim::collect_unit_objects(&mut medium, record).unwrap(),
                })
                .collect();
            let units: Vec<crate::reclaim::ReclaimUnit> = match doctor {
                0 => genuine
                    .iter()
                    .cloned()
                    .chain(std::iter::once(crate::reclaim::ReclaimUnit {
                        record: r1,
                        objects: crate::reclaim::collect_unit_objects(&mut medium, &r1).unwrap(),
                    }))
                    .collect(),
                1 => genuine[1..].to_vec(),
                _ => genuine.iter().rev().cloned().collect(),
            };
            let doctored = crate::reclaim::ReclamationLedgerV2 {
                mk,
                sk: plan.sk,
                batch: batch.index,
                range_digest: plan.range_digest,
                seed: plan.seed,
                records: units.len() as u64,
                reclaimed_keys: 0,
                units,
            };
            let object = StoredObject::new(doctored.canonical_bytes()).unwrap();
            v2x_install_under_key(
                &mut medium,
                crate::reclaim::reclamation_ledger_v2_idempotency_key(&mk, batch.index),
                &object,
                130,
            );
            let error =
                crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "v2-readiness/audit-order"
                    }
                ),
                "{label}: the membership/order violation refuses at the order check: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_v2_reader_refuses_the_falsified_closure_manifest() {
        // (V2X-CP1) The original-evidence equality: an adversarially
        // installed batch-0 audit (the genuine publication skipped,
        // the key occupied by the doctored form) whose unit objects
        // differ from the RECOMPUTED closure (the direct references
        // plus the verified composite closure) refuses typed — a
        // plausible manifest is never authority. The record sequence
        // is genuine (the order and binding checks pass) and the
        // limit is generous (the byte-limit check passes): the
        // closure equality is the failing gate.
        let (root, journal_root, mut medium, mk, _r1, _r2, _shared, private) =
            v2_plan_campaign("v2x-cp1-closure");
        let plan = v2x_publish_all_except(&mut medium, &mk, &journal_root, 2048, 0);
        let batch = &plan.batches[0];
        let genuine: Vec<crate::reclaim::ReclaimUnit> = batch
            .records
            .iter()
            .map(|record| crate::reclaim::ReclaimUnit {
                record: *record,
                objects: crate::reclaim::collect_unit_objects(&mut medium, record).unwrap(),
            })
            .collect();
        // The falsified form: the first unit's objects with a foreign
        // object injected (not a duplicate — the codec's sharing
        // rules pass; the recomputation exposes the fabrication).
        let mut falsified = genuine.clone();
        falsified[0].objects.push(private);
        let doctored = crate::reclaim::ReclamationLedgerV2 {
            mk,
            sk: plan.sk,
            batch: batch.index,
            range_digest: plan.range_digest,
            seed: plan.seed,
            records: falsified.len() as u64,
            reclaimed_keys: 0,
            units: falsified,
        };
        let object = StoredObject::new(doctored.canonical_bytes()).unwrap();
        v2x_install_under_key(
            &mut medium,
            crate::reclaim::reclamation_ledger_v2_idempotency_key(&mk, batch.index),
            &object,
            140,
        );
        let error = crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "v2-readiness/unit-closure-mismatch"
                }
            ),
            "the falsified closure refuses: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_reader_classifies_the_absent_range_without_a_begin() {
        // (V2X-CP1) Option C strict: with the complete plan/audit
        // set installed and NO execution begin in existence, an
        // absent retirement evidence member (a range record, or a
        // content object) is the RANGE-NOT-INTACT classification —
        // never certified ready, never silently crossed — and the
        // reader remains strictly read-only in the diagnostic.
        let (root, _journal_root, medium, mk, r1, _r2, _shared, private) =
            v2_plan_campaign("v2x-cp1-absent");
        {
            let (root_a, journal_root_a, mut medium_a, mk_a, r1_a, _r2a, _s, _p) =
                v2_plan_campaign("v2x-cp1-absent-record");
            let (_plan_id_a, _plan_a, _audit_ids_a) =
                v2x_publish_all(&mut medium_a, &mk_a, &journal_root_a, 400);
            let head_before = medium_a.head().unwrap().unwrap();
            std::fs::remove_file(medium_a.object_path(&r1_a)).unwrap();
            match crate::reclaim::verify_v2_execution_readiness(&mut medium_a, &mk_a).unwrap() {
                crate::reclaim::V2ExecutionReadiness::RangeNotIntact { absent } => {
                    assert!(
                        absent.contains(&r1_a),
                        "the absent range record is named: {absent:?}"
                    );
                }
                other => panic!("the absent record classifies not-intact: {other:?}"),
            }
            assert_eq!(
                medium_a.head().unwrap().unwrap(),
                head_before,
                "the diagnostic reader published nothing"
            );
            let _ = std::fs::remove_dir_all(&root_a);
        }
        {
            let (root_b, journal_root_b, mut medium_b, mk_b, _r1b, _r2b, _s, private_b) =
                v2_plan_campaign("v2x-cp1-absent-object");
            let (_plan_id_b, _plan_b, _audit_ids_b) =
                v2x_publish_all(&mut medium_b, &mk_b, &journal_root_b, 400);
            std::fs::remove_file(medium_b.object_path(&private_b)).unwrap();
            match crate::reclaim::verify_v2_execution_readiness(&mut medium_b, &mk_b).unwrap() {
                crate::reclaim::V2ExecutionReadiness::RangeNotIntact { absent } => {
                    assert!(
                        absent.contains(&private_b),
                        "the absent content object is named: {absent:?}"
                    );
                }
                other => panic!("the absent object classifies not-intact: {other:?}"),
            }
            let _ = std::fs::remove_dir_all(&root_b);
        }
        let _ = (root, r1, private, mk, medium);
    }

    #[test]
    fn the_v2_reader_discovery_is_equivalent_to_the_per_key_lookups() {
        // (V2X-CP1) §3: the single-descent multi-key discovery gives
        // the SAME canonical membership results as the accepted
        // per-key lookups, on the intact (skip-bearing) chain — and
        // the early exit fires on the partial key set.
        let (root, journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
            v2_plan_campaign("v2x-cp1-discovery");
        let (plan_id, plan, audit_ids) = v2x_publish_all(&mut medium, &mk, &journal_root, 400);
        let mut expected: std::collections::BTreeSet<Vec<u8>> =
            std::collections::BTreeSet::from([crate::reclaim::reclamation_plan_idempotency_key(
                &mk,
            )]);
        for batch in plan.batches.iter().map(|b| b.index) {
            expected.insert(crate::reclaim::reclamation_ledger_v2_idempotency_key(
                &mk, batch,
            ));
        }
        let discovered = crate::reclaim::discover_keyed_set(&mut medium, &expected).unwrap();
        assert_eq!(discovered.len(), expected.len(), "every key discovered");
        // The plan equivalence: the same record identity, and the
        // locator's object hash IS the plan payload's content
        // identity (the payload itself is NOT retained — the F1
        // bounded-memory contract).
        let (per_key_id, plan_object) = discovered
            .get(&crate::reclaim::reclamation_plan_idempotency_key(&mk))
            .unwrap();
        assert_eq!(*per_key_id, plan_id);
        assert_eq!(
            *plan_object,
            StoredObject::new(plan.canonical_bytes()).unwrap().hash()
        );
        // The audit equivalence (every batch): the same record
        // identities, and each locator's object hash equals the
        // per-key lookup's decoded ledger's content identity.
        for (batch, audit_id) in plan.batches.iter().map(|b| b.index).zip(&audit_ids) {
            let (found_id, object) = discovered
                .get(&crate::reclaim::reclamation_ledger_v2_idempotency_key(
                    &mk, batch,
                ))
                .unwrap();
            assert_eq!(*found_id, *audit_id);
            let (per_key_id, per_key_ledger) =
                crate::reclaim::find_installed_v2_audit(&mut medium, &mk, batch)
                    .unwrap()
                    .unwrap();
            assert_eq!(*found_id, per_key_id);
            assert_eq!(
                *object,
                StoredObject::new(per_key_ledger.canonical_bytes())
                    .unwrap()
                    .hash()
            );
        }
        // THE EARLY EXIT: a single-key expected set finds its target
        // and returns without descending further.
        let single = crate::reclaim::discover_keyed_set(
            &mut medium,
            &std::collections::BTreeSet::from([
                crate::reclaim::reclamation_ledger_v2_idempotency_key(&mk, plan.batches[0].index),
            ]),
        )
        .unwrap();
        assert_eq!(single.len(), 1, "the early exit returns the one match");
        // A MISSING key in the set: the walk terminates at the
        // founding with the found subset (the completeness check is
        // the caller's).
        let with_missing = crate::reclaim::discover_keyed_set(
            &mut medium,
            &std::collections::BTreeSet::from([
                crate::reclaim::reclamation_ledger_v2_idempotency_key(&mk, 0),
                crate::reclaim::reclamation_ledger_v2_idempotency_key(&mk, 99),
            ]),
        )
        .unwrap();
        assert!(
            with_missing.len() == 1,
            "the genuinely-absent key is not fabricated: {:?}",
            with_missing.len()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_reader_refuses_the_corrupt_and_unsupported_evidence() {
        // (V2X-CP1) The corruption and unsupported-protocol classes
        // are typed failures, never classifications: (a) corrupt
        // content under the correct content-addressed name (the
        // hash mismatch propagates); (b) a malformed wire version
        // under the plan key (the strict decode refuses).
        let (root, journal_root, mut medium, mk, _r1, _r2, _shared, private) =
            v2_plan_campaign("v2x-cp1-corrupt");
        let (_plan_id, _plan, _audit_ids) = v2x_publish_all(&mut medium, &mk, &journal_root, 400);
        // (a) The corrupt object bytes under the correct name: the
        // hash-verifying read refuses (not the not-intact
        // classification — corruption is a failure).
        std::fs::write(medium.object_path(&private), b"corrupted-bytes").unwrap();
        let error = crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap_err();
        assert!(
            matches!(error, MediumError::HashMismatch { .. }),
            "the corrupt content refuses typed: {error:?}"
        );
        // (b) The malformed version: a plan object carrying an
        // unsupported version under the plan key.
        {
            let (root_b, _journal_root_b, mut medium_b, mk_b, _r1b, _r2b, _s, _p) =
                v2_plan_campaign("v2x-cp1-version");
            let mut bytes = {
                let plan = crate::reclaim::ReclaimPlan {
                    mk: mk_b,
                    sk: _r2b,
                    range_digest: [0u8; 32],
                    seed: [0u8; 32],
                    ledger_byte_limit: 1024,
                    batches: vec![crate::reclaim::PlanBatch {
                        index: 0,
                        records: vec![_r1b],
                    }],
                    journal_files: Vec::new(),
                };
                plan.canonical_bytes()
            };
            // The version follows the domain tag (the length-
            // prefixed string + the u32): flip it to an unsupported
            // value in the encoded bytes.
            let domain_len = crate::reclaim::RECLAMATION_PLAN_DOMAIN.len() as u64;
            let version_at = 8 + domain_len as usize;
            bytes[version_at..version_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let object = StoredObject::new(bytes).unwrap();
            v2x_install_under_key(
                &mut medium_b,
                crate::reclaim::reclamation_plan_idempotency_key(&mk_b),
                &object,
                150,
            );
            let error =
                crate::reclaim::verify_v2_execution_readiness(&mut medium_b, &mk_b).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Format(engine_core::SaveError::Version { .. })
                ),
                "the unsupported version refuses typed: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root_b);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_reader_classifies_the_combined_incomplete_publication_and_degraded_range() {
        // (V2X-CP1 F2) THE TRUTHFUL CLASSIFICATION of the combined
        // state: a genuine plan with only a SUBSET of its audits
        // published AND an original retirement record removed. The
        // audit-set discovery's descent cannot cross the degraded
        // range, so the completeness cannot be established — the
        // truthful answer is the TYPED REFUSAL asserting NEITHER
        // the audit-set completeness NOR the readiness (never a
        // false PublicationIncomplete claiming the missing set is
        // known; never a RangeNotIntact claiming the complete audit
        // set; and NEVER ExecutionReady).
        let (root, journal_root, mut medium, mk, r1, _r2, _shared, _private) =
            v2_plan_campaign("v2x-f2-combined");
        // The plan + batch 0 only (the limit 460 splits the range
        // into two batches; batch 1's publication skipped).
        let plan = v2x_publish_all_except(&mut medium, &mk, &journal_root, 460, 1);
        assert!(
            plan.batches.len() >= 2,
            "the fixture splits into multiple batches"
        );
        // The original retirement record removed (the degraded
        // range — on its own this would classify RangeNotIntact
        // under the complete-audit set).
        std::fs::remove_file(medium.object_path(&r1)).unwrap();
        let head_before = medium.head().unwrap().unwrap();
        let error = crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "v2-readiness/range-degraded-before-completeness"
                }
            ),
            "the combined state refuses typed, asserting neither completeness nor readiness: {error:?}"
        );
        // The reader remains strictly read-only in the diagnostic.
        assert_eq!(
            medium.head().unwrap().unwrap(),
            head_before,
            "the reader published nothing"
        );
        // The control: with EVERY audit published and the same
        // record removed, the classification IS the degraded range
        // (the completeness-first ordering never masks it).
        {
            let (root_b, journal_root_b, mut medium_b, mk_b, r1_b, _r2b, _s, _p) =
                v2_plan_campaign("v2x-f2-combined-control");
            let (_plan_id_b, _plan_b, _audit_ids_b) =
                v2x_publish_all(&mut medium_b, &mk_b, &journal_root_b, 400);
            std::fs::remove_file(medium_b.object_path(&r1_b)).unwrap();
            match crate::reclaim::verify_v2_execution_readiness(&mut medium_b, &mk_b).unwrap() {
                crate::reclaim::V2ExecutionReadiness::RangeNotIntact { absent } => {
                    assert!(
                        absent.contains(&r1_b),
                        "the absent record is named: {absent:?}"
                    );
                }
                other => panic!("the complete-audit degraded range classifies: {other:?}"),
            }
            let _ = std::fs::remove_dir_all(&root_b);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_v2_reader_verifies_the_multi_batch_audit_set_sequentially() {
        // (V2X-CP1 F1) THE BOUNDED-MEMORY CONTRACT'S
        // REPRESENTATIVE EVIDENCE: a three-batch campaign whose
        // audits' AGGREGATE encoded size exceeds any individual
        // batch's — the reader certifies through the per-audit
        // RELOAD path (each audit's record and object re-read and
        // hash-verified at its turn, the payload and decoded
        // closure released before the next), never by retaining
        // every audit's raw payload simultaneously (the discovery
        // carries only the record identities and the content-object
        // hashes — the structural bound, pinned by the equivalence
        // regression). The incremental range digest's PARITY with
        // the accepted slice algorithm is asserted directly.
        let (root, journal_root, mut medium, mk, _r1, _r2, _shared, _private) =
            v2_plan_campaign("v2x-f1-sequential");
        let (plan_id, plan, audit_ids) = v2x_publish_all(&mut medium, &mk, &journal_root, 400);
        assert!(
            plan.batches.len() >= 3,
            "the fixture splits into at least three batches"
        );
        // The aggregate volume exceeds each individual audit (the
        // representative multi-audit evidence).
        let mut sizes = Vec::new();
        for batch in plan.batches.iter().map(|b| b.index) {
            let (_id, ledger) = crate::reclaim::find_installed_v2_audit(&mut medium, &mk, batch)
                .unwrap()
                .unwrap();
            sizes.push(ledger.canonical_bytes().len());
        }
        let aggregate: usize = sizes.iter().sum();
        assert!(
            sizes.iter().all(|size| aggregate > *size),
            "the aggregate encoded size exceeds each individual audit: {sizes:?}"
        );
        // THE CERTIFICATION through the reload path: the exact
        // revision identities (the discovery's locators, each
        // re-read and hash-verified at its turn).
        match crate::reclaim::verify_v2_execution_readiness(&mut medium, &mk).unwrap() {
            crate::reclaim::V2ExecutionReadiness::ExecutionReady {
                plan_record,
                audit_records,
                ..
            } => {
                assert_eq!(plan_record, plan_id);
                assert_eq!(audit_records, audit_ids);
            }
            other => panic!("the sequential multi-batch state certifies ready: {other:?}"),
        }
        // THE DIGEST PARITY: the incremental chain_range_digest_step
        // fold equals the accepted chain_range_digest slice
        // algorithm over the same members (the reader's bounded fold
        // is byte-exact with the accepted implementation).
        {
            // The members: the campaign's r1/r2 canonical bytes,
            // oldest first (the fixture's own digest inputs).
            let (root_c, journal_root_c, mut medium_c, mk_c, r1_c, r2_c, _s, _p) =
                v2_plan_campaign("v2x-f1-sequential-parity");
            let founding = crate::reclaim::founding_identity_medium(&mut medium_c).unwrap();
            let seed = crate::checkpoint::compute_digest_seed(&founding);
            let members: Vec<Vec<u8>> = [r1_c, r2_c]
                .iter()
                .map(|id| medium_c.read_record(id).unwrap().canonical_bytes())
                .collect();
            let accepted = crate::checkpoint::chain_range_digest(&seed, &members);
            let mut incremental = seed;
            for bytes in &members {
                incremental = crate::checkpoint::chain_range_digest_step(&incremental, bytes);
            }
            assert_eq!(
                accepted, incremental,
                "the incremental fold is byte-exact with the accepted slice algorithm"
            );
            // The reader's own certification over the same campaign
            // proves the end-to-end parity (the fixture's stored
            // digest was computed by the ACCEPTED algorithm).
            let (_plan_id_c, _plan_c, _audit_ids_c) =
                v2x_publish_all(&mut medium_c, &mk_c, &journal_root_c, 400);
            match crate::reclaim::verify_v2_execution_readiness(&mut medium_c, &mk_c).unwrap() {
                crate::reclaim::V2ExecutionReadiness::ExecutionReady { .. } => {}
                other => panic!("the parity campaign certifies ready: {other:?}"),
            }
            let _ = std::fs::remove_dir_all(&root_c);
        }
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
        // (OC-C3 CP-2) A's begin — the REAL trusted admission (the
        // fixture's executions require it under the begin-bound
        // gate; on a ProcessCrashOnly platform the admission refuses
        // and the dependent tests assert the typed execution refusal
        // instead — the platform policy is unchanged). (OC-C4) The
        // begin record is B1's TRUE nearest retained ancestor: the
        // later range's skip entry must name it, never audit_a — a
        // gap-crossing walk from MK2 lands on the begin, never
        // bypasses it (the real crossing-built tables name the true
        // nearest retained record the same way).
        let mut begin_a_id = audit_a;
        if medium.durability_tier() == crate::medium::SyncTier::FullPosixSequence {
            let begin_outcome = medium.begin_reclamation(&audit_a, &mk1.id(), 45).unwrap();
            assert!(
                matches!(
                    begin_outcome,
                    crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                ),
                "the fixture's A begin admits: {begin_outcome:?}"
            );
            if let crate::reclaim::BeginPublicationOutcome::Accepted { id, .. } = begin_outcome {
                begin_a_id = id;
            }
        }
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
        // (OC-C3 CP-2) The Windows platform policy: ProcessCrashOnly
        // refuses the destructive authorization — the typed
        // execution refusal with nothing deleted (the substance
        // covered on POSIX and the SQLite suites).
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.reclaim(&directive_a).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "execution/begin-tier-insufficient"
                    }
                ),
                "ProcessCrashOnly refuses the destructive grade: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
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
        // (OC-C3 CP-2) The begin-bound migration: the real begin on
        // the eligible tier; the typed platform refusal otherwise.
        if !authorize_or_refuse(&mut medium, &audit_a, &mk1, &directive_a, 206, &root) {
            return;
        }
        medium.reclaim(&directive_a).unwrap();
        assert!(medium.object_path(&x).is_file(), "X survives A (deferred)");
        // The previously installed audit for B (above A's range,
        // never yet executed).
        let (audit_b, outcome) = publish_reclamation_ledger(&mut medium, &ledger_b, 8).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        // (OC-C3 CP-2) B's begin — the real admission (the
        // interleaved execution inside A's completion window
        // requires it under the begin-bound gate).
        let begin_b = medium
            .begin_reclamation(&audit_b, &ledger_b.mk, 47)
            .unwrap();
        assert!(
            matches!(
                begin_b,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "B's begin admits: {begin_b:?}"
        );
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
            fn begin_reclamation(
                &mut self,
                audit: &Hash,
                mk: &Hash,
                tick: u64,
            ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
                // (OC-C3 CP-1) The double's plain delegation.
                self.inner.begin_reclamation(audit, mk, tick)
            }

            fn verify_begin_authority(
                &mut self,
                audit: &Hash,
                mk: &Hash,
            ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
                // (OC-C3 CP-1) The double's plain delegation.
                self.inner.verify_begin_authority(audit, mk)
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
        // (OC-C3 CP-2) The Windows platform policy: ProcessCrashOnly
        // refuses the destructive authorization — the typed
        // execution refusal with nothing deleted (the substance
        // covered on POSIX and the SQLite suites).
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.reclaim(&directive_a).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "execution/begin-tier-insufficient"
                    }
                ),
                "ProcessCrashOnly refuses the destructive grade: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        medium.reclaim(&directive_a).unwrap();
        let (marker_a, _marker) =
            crate::reclaim::complete_reclamation(&mut medium, &audit_a, &mk1).unwrap();
        let (audit_b, outcome) = publish_reclamation_ledger(&mut medium, &ledger_b, 8).unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        // (OC-C3 CP-2) B's begin — the real admission.
        let begin_b = medium
            .begin_reclamation(&audit_b, &ledger_b.mk, 48)
            .unwrap();
        assert!(
            matches!(
                begin_b,
                crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                    | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
            ),
            "B's begin admits: {begin_b:?}"
        );
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

    #[test]
    fn the_oc4_beginless_later_marker_never_relaxes_an_earlier_certification() {
        // (OC-C4) The completion-reader authority at the CASCADE
        // level: a later marker without begin authority never relaxes
        // an earlier certification — its canonical membership, valid
        // encoding, authenticated audit, and internally consistent
        // obligations never substitute for trusted begin provenance.
        // PHASE 1 (the mid-flight shape): A genuinely completed (the
        // fixture's real begin, the execution, and the real marker —
        // the shared X deferred); B's retirement HAND-satisfied with
        // the CORRECT marker content but NO B begin — A's trusted
        // verification refuses at B's begin absence. PHASE 2 (the
        // positive control): the same shape with B's REAL begin
        // admitted before the hand satisfaction — A's verification
        // passes (the legitimate historical attribution through
        // B's authenticated completion). On Windows the fixture's A
        // execution refuses the platform tier (the independent
        // ProcessCrashOnly proof — no destructive authorization
        // exists to certify or relax).
        let install =
            |medium: &mut LocalFileMedium, marker: &crate::reclaim::ReclamationComplete| -> Hash {
                let head = medium.head().unwrap().unwrap();
                let parent_epoch = medium.read_record(&head).unwrap().epoch;
                let key = crate::reclaim::reclamation_complete_idempotency_key(
                    &marker.mk,
                    Some(marker.batch),
                );
                let object = StoredObject::new(marker.canonical_bytes()).unwrap();
                let record = RevisionRecord::regular(head, parent_epoch, 98)
                    .with_idempotency_key(key)
                    .with_content_refs(vec![object.hash()]);
                let admitted =
                    crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
                medium.append_admitted(admitted, Some(head)).unwrap();
                record.id()
            };
        for admit_b_begin in [false, true] {
            let label = if admit_b_begin {
                "oc4-later-positive"
            } else {
                "oc4-later-beginless"
            };
            let (root, mut medium, audit_a, mk1, _mk2, x, fobj, directive_a, ledger_b) =
                two_retirement_store(label);
            if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
                // Windows: the A execution refuses the platform tier.
                let error = medium.reclaim(&directive_a).unwrap_err();
                assert!(
                    matches!(
                        error,
                        MediumError::Chain {
                            rule: "execution/begin-tier-insufficient"
                        }
                    ),
                    "ProcessCrashOnly refuses the destructive grade: {error:?}"
                );
                let _ = std::fs::remove_dir_all(&root);
                continue;
            }
            // A: the genuine completion (the fixture's begin, the
            // execution, the real marker through the wrapper).
            medium.reclaim(&directive_a).unwrap();
            let (marker_a, marker_a_content) =
                crate::reclaim::complete_reclamation(&mut medium, &audit_a, &mk1).unwrap();
            assert!(
                marker_a_content.deferred.contains(&x),
                "A defers the shared X (B1, a survivor above A, references it)"
            );
            // B's audit.
            let (audit_b, _) = publish_reclamation_ledger(&mut medium, &ledger_b, 8).unwrap();
            // (OC-C4 phase 2) B's REAL begin — admitted BEFORE the
            // hand satisfaction (the first-execution evidence still
            // present); phase 1 publishes none.
            if admit_b_begin {
                let begin_b = medium
                    .begin_reclamation(&audit_b, &ledger_b.mk, 48)
                    .unwrap();
                assert!(
                    matches!(
                        begin_b,
                        crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                            | crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }
                    ),
                    "B's begin admits: {begin_b:?}"
                );
            }
            // B's retirement HAND-SATISFIED: the mandatory object
            // deleted while the records still reference it, then the
            // unit records (the real execution's end state).
            std::fs::remove_file(medium.object_path(&x)).unwrap();
            for unit in &ledger_b.units {
                std::fs::remove_file(medium.object_path(&unit.record)).unwrap();
            }
            // B's CORRECT marker content (the completion state's own
            // oracle — the obligations genuinely met).
            let (mandatory_count, deferred) =
                crate::reclaim::verify_completion_state(&mut medium, &ledger_b).unwrap();
            let marker_b_content = crate::reclaim::ReclamationComplete {
                kind: crate::reclaim::CompletionKind::Batch,
                mk: ledger_b.mk,
                batch: 0,
                source: audit_b,
                mandatory_count,
                deferred: deferred.into_iter().collect(),
                journal_files: Vec::new(),
            };
            let _marker_b_id = install(&mut medium, &marker_b_content);
            // A's trusted verification.
            if admit_b_begin {
                // THE POSITIVE CONTROL: B's authenticated completion
                // resolves A's deferred accounting — the legitimate
                // historical attribution.
                crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1).unwrap();
            } else {
                // THE REFUSAL: B's beginless marker never relaxes A's
                // certification (A's own begin IS present — the
                // refusal is the later marker's by elimination).
                medium
                    .verify_begin_authority(&audit_a, &mk1)
                    .expect("A's own begin authority holds");
                let error =
                    crate::reclaim::verify_reclamation_complete(&mut medium, &marker_a, &mk1)
                        .unwrap_err();
                assert!(
                    matches!(
                        error,
                        MediumError::Chain {
                            rule: "begin-authority/absent"
                        }
                    ),
                    "the beginless later marker never relaxes an earlier certification: {error:?}"
                );
            }
            let _ = fobj;
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    #[cfg(feature = "composite-test-fixture")]
    fn the_oc4_local_completion_reader_tier_gate() {
        // (OC-C4) The insufficient-tier refusal through the
        // completion reader: on Windows (ProcessCrashOnly) even a
        // correctly shaped, correctly bound, ON-CHAIN begin record
        // cannot authorize trusted certification — the platform tier
        // gate inside verify_begin_authority refuses and propagates
        // through the reader (the OC-P2 rule: an installed, restored,
        // or otherwise discoverable begin never overrides the
        // platform limitation). On POSIX the same hand-installed
        // valid begin passes the discovery, binding, reaffirmation,
        // and tier — and the correctly satisfied retirement's marker
        // CERTIFIES (the local positive through the reader).
        let (root, mut medium, audit, mk, shared, private) = trusted_execution_store();
        let (ledger, _meta) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
        // The hand-installed begin (the right deterministic key, the
        // correct four-field binding — bypassing the admission,
        // exactly the restored/legacy record the tier gate must
        // refuse on Windows).
        let begin = crate::reclaim::ReclamationBegin {
            mk,
            source: audit,
            range_digest: ledger.range_digest,
            seed: ledger.seed,
        };
        let _begin_id =
            crate::reclaim::test_only_install_begin_record(&mut medium, &begin, 240).unwrap();
        // The satisfied retirement (the hand construction: the
        // mandatory object deleted while the records still reference
        // it, then the unit records).
        std::fs::remove_file(medium.object_path(&private)).unwrap();
        for unit in &ledger.units {
            std::fs::remove_file(medium.object_path(&unit.record)).unwrap();
        }
        // The correct marker content (the completion state's own
        // oracle).
        let (mandatory_count, deferred) =
            crate::reclaim::verify_completion_state(&mut medium, &ledger).unwrap();
        let marker = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk,
            batch: 0,
            source: audit,
            mandatory_count,
            deferred: deferred.into_iter().collect(),
            journal_files: Vec::new(),
        };
        let marker_id =
            crate::reclaim::test_only_install_completion_marker(&mut medium, &marker, 241).unwrap();
        let records_before = medium.validate().unwrap().records;
        // THE READER'S VERDICT.
        let result = crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk);
        if cfg!(windows) {
            match result {
                Err(MediumError::Chain {
                    rule: "begin-authority/tier-insufficient",
                }) => {}
                other => panic!(
                    "the Windows platform tier refuses the reader's certification: {other:?}"
                ),
            }
        } else {
            result.expect("the correctly authorized and satisfied retirement certifies");
        }
        // No forbidden effect either way: the verification published
        // nothing (the record count unchanged) and the survivor
        // content remains present and hash-valid.
        let records_after = medium.validate().unwrap().records;
        assert_eq!(
            records_after, records_before,
            "the verification published nothing"
        );
        medium.read_object(&shared).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_completion_unknown_and_duplicate_reconciliation_branches() {
        // (T1, the final reconciliation evidence) The ACTUAL
        // complete_reclamation control flow over a test-only
        // CanonicalMedium wrapper (a real LocalFileMedium beneath —
        // the authenticated audit and storage state are genuine;
        // only the append/reconcile outcomes are injected):
        // Unknown→Installed (the real append happened; the
        // reconciliation discovers the installed candidate — the
        // correct marker, no duplication); Unknown→NotInstalled (the
        // retry proceeds through the correct admission and CAS —
        // installed ONCE); Unknown→Unverifiable (the helper refuses
        // without fabricating completion); and the duplicate key
        // with MISMATCHING content (the discovery-and-exact-verify
        // path refuses — the key's presence is never success).
        enum Inject {
            Installed,
            NotInstalled,
            Unverifiable,
        }
        struct UnknownMedium<'a> {
            inner: &'a mut LocalFileMedium,
            inject: Inject,
            armed: bool,
        }
        impl CanonicalMedium for UnknownMedium<'_> {
            fn head(&mut self) -> Result<Option<Hash>, MediumError> {
                self.inner.head()
            }
            fn append_admitted(
                &mut self,
                admitted: crate::medium::AdmittedAppend,
                expected_parent: Option<Hash>,
            ) -> Result<AppendOutcome, MediumError> {
                if !self.armed {
                    return self.inner.append_admitted(admitted, expected_parent);
                }
                self.armed = false;
                match self.inject {
                    Inject::Installed => {
                        // The REAL append happens; the outcome is the
                        // injected uncertain commit.
                        self.inner.append_admitted(admitted, expected_parent)?;
                        Ok(AppendOutcome::Unknown {
                            reason: crate::medium::UnknownReason::CommitUncertain,
                        })
                    }
                    Inject::NotInstalled => {
                        // The append never happens; the retry must
                        // proceed through the correct admission.
                        Ok(AppendOutcome::Unknown {
                            reason: crate::medium::UnknownReason::ObjectWriteFailed,
                        })
                    }
                    Inject::Unverifiable => Ok(AppendOutcome::Unknown {
                        reason: crate::medium::UnknownReason::HeadPublishFailed,
                    }),
                }
            }
            fn reconcile_unknown(
                &mut self,
                candidate: &Hash,
            ) -> Result<crate::medium::Reconciliation, MediumError> {
                match self.inject {
                    Inject::Installed => self.inner.reconcile_unknown(candidate),
                    Inject::NotInstalled => Ok(crate::medium::Reconciliation::NotInstalled {
                        head: self.inner.head()?,
                    }),
                    Inject::Unverifiable => Ok(crate::medium::Reconciliation::Unverifiable {
                        reason: "the injected uncertain outcome".to_owned(),
                    }),
                }
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
            fn begin_reclamation(
                &mut self,
                audit: &Hash,
                mk: &Hash,
                tick: u64,
            ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
                // (OC-C3 CP-1) The double's plain delegation.
                self.inner.begin_reclamation(audit, mk, tick)
            }

            fn verify_begin_authority(
                &mut self,
                audit: &Hash,
                mk: &Hash,
            ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
                // (OC-C3 CP-1) The double's plain delegation.
                self.inner.verify_begin_authority(audit, mk)
            }
        }
        let marker_domain_records = |medium: &mut LocalFileMedium| -> usize {
            // Count the completion-domain records on the chain.
            let mut current = medium.head().unwrap().unwrap();
            let mut count = 0usize;
            loop {
                let record = medium.read_record(&current).unwrap();
                if let Some(object) = record.content_refs.first()
                    && let Ok(bytes) = medium.read_object(object)
                    && StoredObject::new(bytes)
                        .map(|o| o.domain() == crate::reclaim::RECLAMATION_COMPLETE_DOMAIN)
                        .unwrap_or(false)
                {
                    count += 1;
                }
                // The walk stops at the reclaimed gap — the
                // markers all live above it.
                match record.parent {
                    Some(parent) if medium.read_record(&parent).is_ok() => current = parent,
                    _ => break,
                }
            }
            count
        };
        // (1) Unknown → Installed: the real append committed; the
        // reconciliation discovers it; the helper returns the marker
        // with NO duplication.
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            // (OC-C3 CP-2) The begin-bound migration.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 212, &root) {
                return;
            }
            // (OC-C3 CP-2) The begin-bound migration.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 212, &root) {
                return;
            }
            medium.reclaim(&directive).unwrap();
            let (marker_id, marker) = {
                let mut wrapper = UnknownMedium {
                    inner: &mut medium,
                    inject: Inject::Installed,
                    armed: true,
                };
                crate::reclaim::complete_reclamation(&mut wrapper, &audit, &mk).unwrap()
            };
            assert_eq!(marker.kind, crate::reclaim::CompletionKind::Batch);
            assert_eq!(marker.source, audit);
            assert_eq!(
                marker_domain_records(&mut medium),
                1,
                "exactly one completion marker — no duplication"
            );
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk).unwrap();
            let _ = std::fs::remove_dir_all(&root);
        }
        // (2) Unknown → NotInstalled: the retry proceeds through the
        // correct admission and CAS; installed ONCE.
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            // (OC-C3 CP-2) The begin-bound migration.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 212, &root) {
                return;
            }
            medium.reclaim(&directive).unwrap();
            let (marker_id, marker) = {
                let mut wrapper = UnknownMedium {
                    inner: &mut medium,
                    inject: Inject::NotInstalled,
                    armed: true,
                };
                crate::reclaim::complete_reclamation(&mut wrapper, &audit, &mk).unwrap()
            };
            assert_eq!(marker.source, audit);
            assert_eq!(
                marker_domain_records(&mut medium),
                1,
                "the retry installed the marker exactly once"
            );
            crate::reclaim::verify_reclamation_complete(&mut medium, &marker_id, &mk).unwrap();
            let _ = std::fs::remove_dir_all(&root);
        }
        // (3) Unknown → Unverifiable: the helper refuses without
        // fabricating completion; nothing is installed.
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            // (OC-C3 CP-2) The begin-bound migration.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 212, &root) {
                return;
            }
            medium.reclaim(&directive).unwrap();
            let error = {
                let mut wrapper = UnknownMedium {
                    inner: &mut medium,
                    inject: Inject::Unverifiable,
                    armed: true,
                };
                crate::reclaim::complete_reclamation(&mut wrapper, &audit, &mk).unwrap_err()
            };
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-complete/unknown-unverifiable"
                    }
                ),
                "the unverifiable outcome refuses: {error:?}"
            );
            assert_eq!(
                marker_domain_records(&mut medium),
                0,
                "nothing was installed"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (4) The duplicate key with MISMATCHING content: the
        // deterministic key is held by an adversarial wrong-content
        // marker; the duplicate-resolution path discovers it, finds
        // the content does not match the verified state, and REFUSES
        // — the key's presence is never success.
        {
            let (root, mut medium, audit, mk, shared, _private) = trusted_execution_store();
            let (ledger, _meta) =
                crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit, &mk).unwrap();
            let directive = crate::reclaim::ReclaimDirective {
                audit_record: audit,
                units: ledger.units.clone(),
            };
            // (OC-C3 CP-2) The begin-bound migration.
            if !authorize_or_refuse(&mut medium, &audit, &mk, &directive, 212, &root) {
                return;
            }
            medium.reclaim(&directive).unwrap();
            // The adversarial wrong-content marker at the SAME
            // deterministic key (the in-crate append — the corrupted-
            // in-crate-path threat).
            let head = medium.head().unwrap().unwrap();
            let parent_epoch = medium.read_record(&head).unwrap().epoch;
            let wrong = crate::reclaim::ReclamationComplete {
                kind: crate::reclaim::CompletionKind::Batch,
                mk,
                batch: 0,
                source: audit,
                mandatory_count: 42,
                deferred: vec![shared],
                journal_files: Vec::new(),
            };
            let object = StoredObject::new(wrong.canonical_bytes()).unwrap();
            let record = RevisionRecord::regular(head, parent_epoch, 97)
                .with_idempotency_key(crate::reclaim::reclamation_complete_idempotency_key(
                    &mk,
                    Some(0),
                ))
                .with_content_refs(vec![object.hash()]);
            let admitted =
                crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
            let outcome = medium.append_admitted(admitted, Some(head)).unwrap();
            assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
            let error = crate::reclaim::complete_reclamation(&mut medium, &audit, &mk).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "reclamation-complete/duplicate-content-mismatch"
                    }
                ),
                "the mismatched duplicate refuses: {error:?}"
            );
            assert_eq!(
                marker_domain_records(&mut medium),
                1,
                "no second marker — only the adversarial one stands, refused"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_trusted_begin_admission_publishes_and_refuses() {
        // (OC-C2) The local trusted admission: the valid fixture
        // publishes Accepted (the begin on-chain, the deterministic
        // key, the HEAD advanced); the repeat call resolves the
        // duplicate by EXACT content verification; the fabricated
        // digest, the missing original record, and the wrong MK all
        // refuse BEFORE any publication (the HEAD unchanged). The
        // admission is NOT connected to the destructive path (the
        // retirement still executable afterward through the existing
        // reclaim).
        let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
        let head_before = medium.head().unwrap().unwrap();
        // (OC-P2) The tier gate FIRST: on a ProcessCrashOnly platform
        // (Windows local) the admission refuses fail-closed — the
        // ratified destructive-durability policy, demonstrated here
        // as the platform's actual behavior (the SQLite twin covers
        // the publication on that platform).
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.begin_reclamation(&audit, &mk, 30).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/tier-insufficient"
                    }
                ),
                "the Windows-local tier refuses: {error:?}"
            );
            assert_eq!(
                medium.head().unwrap().unwrap(),
                head_before,
                "the refusal leaves the HEAD unchanged"
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        // (1) The valid admission.
        let outcome = medium.begin_reclamation(&audit, &mk, 30).unwrap();
        let installed = match outcome {
            // (C2-R8) The POSIX admission carries its ACTUAL tier.
            crate::reclaim::BeginPublicationOutcome::Accepted { id, tier } => {
                assert_eq!(
                    tier,
                    crate::medium::SyncTier::FullPosixSequence,
                    "the POSIX local admission carries its actual tier"
                );
                id
            }
            other => panic!("the valid admission accepts, got {other:?}"),
        };
        let head_after = medium.head().unwrap().unwrap();
        assert_eq!(head_after, installed, "the begin is the new HEAD");
        assert_ne!(head_after, head_before);
        // The discovered begin verifies with the exact bindings.
        let (found_id, found) = crate::reclaim::find_installed_completion(&mut medium, &mk)
            .unwrap()
            .map(|(id, _)| (id, ()))
            .map(|_| (installed, ()))
            .unwrap_or((installed, ()));
        let _ = (found_id, found);
        let begin_record = medium.read_record(&installed).unwrap();
        let begin_object = medium.read_object(&begin_record.content_refs[0]).unwrap();
        let begin = crate::reclaim::ReclamationBegin::decode(&begin_object).unwrap();
        assert_eq!(begin.mk, mk);
        assert_eq!(begin.source, audit);
        // (2) The duplicate: the exact-content verification.
        let outcome = medium.begin_reclamation(&audit, &mk, 31).unwrap();
        match outcome {
            crate::reclaim::BeginPublicationOutcome::DuplicateVerified { id } => {
                assert_eq!(id, installed)
            }
            other => panic!("the duplicate verifies exactly, got {other:?}"),
        }
        // (3) The fabricated digest (the E1 fixture): the admission
        // refuses BEFORE any publication.
        let (root2, mut medium2, audit2, mk2, _s, _p) = {
            // Build the fabricated variant via the established
            // parameterized construction.
            let r = temp_store("occ2-fabricated");
            let mut m = LocalFileMedium::open(r.clone()).unwrap();
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
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, "occ2f", 0)
                .with_content_refs(vec![shared.hash()]);
            m.append(&founding, std::slice::from_ref(&shared), None)
                .unwrap();
            let head = m.head().unwrap().unwrap();
            let epoch = m.read_record(&head).unwrap().epoch;
            let _ = epoch;
            let p1 = RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![shared.hash()]);
            m.append(&p1, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            let head = m.head().unwrap().unwrap();
            let epoch = m.read_record(&head).unwrap().epoch;
            let r1 = RevisionRecord::regular(head, epoch, 2)
                .with_content_refs(vec![shared.hash(), private.hash()]);
            m.append(&r1, &[shared.clone(), private.clone()], Some(head))
                .unwrap();
            let head = m.head().unwrap().unwrap();
            let epoch = m.read_record(&head).unwrap().epoch;
            let sk = RevisionRecord::regular(head, epoch, 3).with_content_refs(vec![shared.hash()]);
            m.append(&sk, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            let seed = crate::checkpoint::compute_digest_seed(&founding.id());
            let r1_bytes = m.read_record(&r1.id()).unwrap().canonical_bytes();
            let real = crate::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
            let fabricated = [9; 32];
            let meta = CheckpointMetadata::new(
                sk.id(),
                r1.id(),
                vec![SkipEntry::new(Hash::ZERO, p1.id())],
                vec![
                    ReclaimedIndexEntry::new(None, r1.id(), 2),
                    ReclaimedIndexEntry::new(None, sk.id(), 3),
                ],
                fabricated,
                seed,
            );
            let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
            let head = m.head().unwrap().unwrap();
            let epoch = m.read_record(&head).unwrap().epoch;
            let mk =
                RevisionRecord::regular(head, epoch, 4).with_content_refs(vec![meta_object.hash()]);
            m.append(&mk, std::slice::from_ref(&meta_object), Some(head))
                .unwrap();
            let r1_refs = m.read_record(&r1.id()).unwrap().content_refs.clone();
            let sk_refs = m.read_record(&sk.id()).unwrap().content_refs.clone();
            let ledger = ReclamationLedger {
                mk: mk.id(),
                sk: sk.id(),
                range_digest: fabricated,
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
            let (audit, outcome) = publish_reclamation_ledger(&mut m, &ledger, 5).unwrap();
            assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
            let _ = real;
            (r, m, audit, mk.id(), shared.hash(), private.hash())
        };
        let head2_before = medium2.head().unwrap().unwrap();
        let error = medium2.begin_reclamation(&audit2, &mk2, 32).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/range-digest-mismatch"
                }
            ),
            "the fabricated commitment refuses: {error:?}"
        );
        assert_eq!(
            medium2.head().unwrap().unwrap(),
            head2_before,
            "the refusal leaves the HEAD unchanged"
        );
        let _ = root2;
        // (4) The missing original record: the deletion-prefix
        // absence is NEVER first-execution authority.
        let (root3, mut medium3, audit3, mk3, _s3, _p3) = trusted_execution_store();
        let (ledger3, _meta3) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium3, &audit3, &mk3).unwrap();
        let missing = ledger3.units[0].record;
        std::fs::remove_file(medium3.object_path(&missing)).unwrap();
        let head3_before = medium3.head().unwrap().unwrap();
        let error = medium3.begin_reclamation(&audit3, &mk3, 33).unwrap_err();
        // The missing original record refuses fail-closed (the
        // broken founding descent or the manifest check — any typed
        // refusal; never an acceptance).
        assert!(
            matches!(
                &error,
                MediumError::MissingObject { .. } | MediumError::Chain { .. }
            ),
            "the missing record refuses typed: {error:?}"
        );
        assert_eq!(medium3.head().unwrap().unwrap(), head3_before);
        // (5) The wrong MK: the mismatch refuses.
        let error = medium
            .begin_reclamation(&audit, &Hash::ZERO, 34)
            .unwrap_err();
        assert!(matches!(error, MediumError::Chain { .. }));
        let _ = (root, root3);
    }

    #[test]
    fn the_begin_admission_authenticates_the_original_content() {
        // (OC-C2-F1/F3/F6) The content-evidence and duplicate-shape
        // regressions: a MISSING referenced content object refuses
        // typed with the HEAD unchanged; a HASH-CORRUPT content
        // object (wrong bytes at the audited identity) refuses
        // typed; an ORPHANED same-key record (off the ancestry) is
        // never discovered as a duplicate — the admission publishes
        // Accepted (the orphan has no authority); a MALFORMED
        // same-key record ON the ancestry (two content refs)
        // refuses the shape validation.
        let (root, mut medium, audit, mk, _shared, private) = trusted_execution_store();
        // (OC-P2) The tier gate: on a ProcessCrashOnly platform the
        // admission refuses fail-closed before any evidence check —
        // the SQLite twin covers the publication there.
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.begin_reclamation(&audit, &mk, 45).unwrap_err();
            assert!(matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/tier-insufficient"
                }
            ));
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        // (F1a) The missing content object.
        std::fs::remove_file(medium.object_path(&private)).unwrap();
        let head_before = medium.head().unwrap().unwrap();
        let error = medium.begin_reclamation(&audit, &mk, 40).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::MissingObject { .. } | MediumError::HashMismatch { .. }
            ),
            "the missing content object refuses: {error:?}"
        );
        assert_eq!(medium.head().unwrap().unwrap(), head_before);
        // (F1b) The hash-corrupt content object (the wrong bytes at
        // the audited identity — the hash-verifying read refuses).
        std::fs::write(medium.object_path(&private), b"corrupted-content").unwrap();
        let error = medium.begin_reclamation(&audit, &mk, 41).unwrap_err();
        assert!(
            matches!(error, MediumError::HashMismatch { .. }),
            "the corrupt content object refuses typed: {error:?}"
        );
        assert_eq!(medium.head().unwrap().unwrap(), head_before);
        // Restore; the admission now succeeds.
        let (root2, mut medium2, audit2, mk2, _s2, _p2) = trusted_execution_store();
        let outcome = medium2.begin_reclamation(&audit2, &mk2, 42).unwrap();
        assert!(matches!(
            outcome,
            crate::reclaim::BeginPublicationOutcome::Accepted {
                tier: crate::medium::SyncTier::FullPosixSequence,
                ..
            }
        ));
        // (F3a) The ORPHANED same-key record: hand-written into the
        // object namespace, NEVER on the ancestry — the discovery
        // (the head descent) must not find it; a fresh store's
        // admission publishes Accepted (the orphan exerts no
        // authority).
        let (root3, mut medium3, audit3, mk3, _s3, _p3) = trusted_execution_store();
        let founding3 = {
            let mut current = medium3.head().unwrap().unwrap();
            loop {
                let bytes = medium3.read_record(&current).unwrap().canonical_bytes();
                let id = crate::hash::hash_object(&bytes);
                let rec = medium3.read_record(&id).unwrap();
                match rec.parent {
                    Some(p) => current = p,
                    None => break id,
                }
            }
        };
        let key = crate::reclaim::reclamation_begin_idempotency_key(&mk3);
        let orphan_object = StoredObject::new(
            crate::reclaim::ReclamationBegin {
                mk: mk3,
                source: audit3,
                range_digest: [0u8; 32],
                seed: [0u8; 32],
            }
            .canonical_bytes(),
        )
        .unwrap();
        let orphan = RevisionRecord::regular(founding3, 0, 99)
            .with_idempotency_key(key)
            .with_content_refs(vec![orphan_object.hash()]);
        std::fs::write(
            medium3.object_path(&orphan_object.hash()),
            orphan_object.bytes(),
        )
        .unwrap();
        std::fs::write(medium3.object_path(&orphan.id()), orphan.canonical_bytes()).unwrap();
        let outcome = medium3.begin_reclamation(&audit3, &mk3, 43).unwrap();
        assert!(
            matches!(
                outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted {
                    tier: crate::medium::SyncTier::FullPosixSequence,
                    ..
                }
            ),
            "the orphaned same-key record never blocks or fakes a duplicate: {outcome:?}"
        );
        // (F3b) The MALFORMED same-key record ON the ancestry: two
        // content refs — the shape validation refuses.
        let (root4, mut medium4, audit4, mk4, s4, _p4) = trusted_execution_store();
        let head4 = medium4.head().unwrap().unwrap();
        let epoch4 = medium4.read_record(&head4).unwrap().epoch;
        let begin4 = crate::reclaim::ReclamationBegin {
            mk: mk4,
            source: audit4,
            range_digest: [0u8; 32],
            seed: [0u8; 32],
        };
        let obj4 = StoredObject::new(begin4.canonical_bytes()).unwrap();
        let malformed = RevisionRecord::regular(head4, epoch4, 98)
            .with_idempotency_key(crate::reclaim::reclamation_begin_idempotency_key(&mk4))
            .with_content_refs(vec![obj4.hash(), s4]);
        let s4_object = medium4.read_object(&s4).unwrap();
        let s4_stored = StoredObject::new(s4_object).unwrap();
        // Install the malformed same-key record ON the ancestry via
        // the in-crate receipt (two content refs — the generic
        // publication does not refuse the count; the begin
        // discovery's shape validation must).
        let admitted =
            crate::medium::AdmittedAppend::generic(&malformed, &[obj4.clone(), s4_stored]);
        medium4.append_admitted(admitted, Some(head4)).unwrap();
        let error = medium4.begin_reclamation(&audit4, &mk4, 44).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/installed-shape-mismatch"
                }
            ),
            "the malformed same-key record refuses the shape validation: {error:?}"
        );
        let _ = (root, root2, root3, root4);
    }

    #[test]
    fn the_begin_admission_founding_crosses_reclaimed_gaps() {
        // (OC-C2-F4) The gap-aware founding: after a legitimate
        // retirement creates an authenticated gap, a LATER
        // checkpoint's begin admission still derives the TRUE
        // founding's seed (the naive missing-parent descent would
        // have stopped at the gap and derived a wrong seed — the
        // refusal); the shared skip-table crossing reaches the real
        // founding and the admission succeeds.
        let (root, mut medium, audit_a, mk1, _mk2, _x1, _x2, _directive_a, _ledger_b) =
            two_retirement_store("occ2-f4");
        // (OC-P2) The tier gate (the Windows-local refusal; the
        // SQLite twin covers the transactional publication).
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.begin_reclamation(&audit_a, &mk1, 55).unwrap_err();
            assert!(matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/tier-insufficient"
                }
            ));
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        // Retirement A executes (the gap below MK1's skip tables).
        let (ledger_a, _meta_a) =
            crate::reclaim::verify_canonical_v1_audit(&mut medium, &audit_a, &mk1).unwrap();
        medium
            .reclaim(&crate::reclaim::ReclaimDirective {
                audit_record: audit_a,
                units: ledger_a.units.clone(),
            })
            .unwrap();
        // The later checkpoint B's audit (already installed by the
        // fixture above A's range).
        // B's audit PUBLISHED now (the fixture returns the ledger
        // unpublished) — above A's retired range, over B's intact
        // records.
        let (audit_b, outcome_b) = publish_reclamation_ledger(&mut medium, &_ledger_b, 9).unwrap();
        assert!(matches!(outcome_b, AppendOutcome::Accepted { .. }));
        // B's admission over the gap-crossed founding: the seed
        // check must pass via the TRUE founding (the gap-aware
        // descent) — the naive descent would fail the seed.
        let outcome = medium.begin_reclamation(&audit_b, &_mk2, 50);
        match outcome {
            Ok(crate::reclaim::BeginPublicationOutcome::Accepted { .. }) => {}
            Ok(crate::reclaim::BeginPublicationOutcome::DuplicateVerified { .. }) => {}
            Ok(crate::reclaim::BeginPublicationOutcome::ReconciledInstalled { .. }) => {}
            Ok(crate::reclaim::BeginPublicationOutcome::ReconciledNotInstalled) => {}
            Err(error) => panic!(
                "the gap-aware founding reaches the TRUE founding (the admission proceeds): {error:?}"
            ),
        }
        let _ = (outcome, root);
    }
    #[test]
    fn the_local_begin_discovery_validates_parent_epoch_linkage() {
        // (C2-R3/R4/R6) The local twin of the linkage regression: a
        // same-key record ON the ancestry whose parent is PRESENT
        // but whose epoch is not parent.epoch+1 refuses at the
        // discovery's linkage — the same coherence the chain
        // validator enforces for every regular revision. The append
        // path enforces the epoch coherence itself, so the wrong-
        // epoch record is installed the corruption-shaped way (the
        // object file, the record file, the HEAD advance) — exactly
        // the rows the linkage check exists for. Exercised through
        // the discovery directly: the (C2-R6) authoritative-chain
        // validation refuses such a HEAD at the public admission
        // boundary (its own regression).
        let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
        // (OC-P2) The tier gate (the Windows ProcessCrashOnly
        // refusal; the SQLite twin covers the transactional
        // publication).
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.begin_reclamation(&audit, &mk, 56).unwrap_err();
            assert!(matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/tier-insufficient"
                }
            ));
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let head = medium.head().unwrap().unwrap();
        let head_epoch = medium.read_record(&head).unwrap().epoch;
        let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
        let begin = crate::reclaim::ReclamationBegin {
            mk,
            source: audit,
            range_digest: [0u8; 32],
            seed: [0u8; 32],
        };
        let object = StoredObject::new(begin.canonical_bytes()).unwrap();
        // The WRONG EPOCH against a PRESENT parent (the struct
        // literal — the constructor enforces parent+1).
        let fake = RevisionRecord {
            kind: crate::record::RecordKind::Regular,
            epoch: head_epoch + 5,
            parent: Some(head),
            idempotency_key: Some(key),
            tick: 92,
            content_refs: vec![object.hash()],
            depends_on: Vec::new(),
        };
        let fake_id = crate::hash::hash_object(&fake.canonical_bytes());
        std::fs::write(medium.object_path_pub(&object.hash()), object.bytes()).unwrap();
        std::fs::write(medium.object_path_pub(&fake_id), fake.canonical_bytes()).unwrap();
        let head_bytes = {
            let mut enc = engine_core::Encoder::new();
            enc.write_str(HEAD_DOMAIN);
            enc.write_u32(HEAD_VERSION);
            enc.write_bytes(fake_id.as_bytes());
            enc.into_bytes()
        };
        std::fs::write(root.join(HEAD_FILE), head_bytes).unwrap();
        let error = medium.find_installed_begin_locked(&mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/installed-linkage-mismatch"
                }
            ),
            "the wrong-epoch same-key record refuses at the linkage: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn the_local_founding_presence_probe_propagates_io_errors() {
        // (C2-R2/R4) The typed presence: a probe error that is NOT
        // NotFound — here a symlink loop at the parent's object path,
        // hit by the founding descent's presence probe before that
        // parent is ever read — propagates as the typed I/O refusal,
        // never converted to a missing-parent observation (which
        // would misread a storage failure as an authenticated gap).
        // The authentication walk stops at the audit and the MK, so
        // the FIRST operation to touch the sabotaged path is the
        // presence probe itself.
        let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
        let mut chain: Vec<Hash> = Vec::new();
        let mut current = medium.head().unwrap().unwrap();
        loop {
            chain.push(current);
            let record = medium.read_record(&current).unwrap();
            match record.parent {
                Some(parent) => current = parent,
                None => break,
            }
        }
        // P1: the second-to-last record on the ancestry (probed
        // during the founding descent, never read before the probe).
        let p1 = chain[chain.len() - 2];
        let p1_path = medium.object_path_pub(&p1);
        std::fs::remove_file(&p1_path).unwrap();
        std::os::unix::fs::symlink(&p1_path, &p1_path).unwrap();
        let error = medium.begin_reclamation(&audit, &mk, 58).unwrap_err();
        assert!(
            matches!(error, MediumError::Io { .. }),
            "the unreadable presence probe propagates the typed I/O refusal (never an absence): {error:?}"
        );
        std::fs::remove_file(&p1_path).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_local_begin_admission_requires_authoritative_chain_coherence() {
        // (C2-R6) The local twin: a chain carrying an otherwise
        // authentic audit and retirement evidence but a MALFORMED
        // revision above the audit never receives a trusted begin —
        // the trusted-cursor validation under the held lock refuses
        // before the early duplicate outcome and before the
        // installation; the valid history still admits.
        let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
        // (OC-P2) The tier gate (the Windows ProcessCrashOnly
        // refusal; the SQLite twin covers the transactional
        // publication).
        if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
            let error = medium.begin_reclamation(&audit, &mk, 59).unwrap_err();
            assert!(matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/tier-insufficient"
                }
            ));
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        // (a) The incorrect regular-revision epoch above the audit —
        // installed the corruption-shaped way (the object file, the
        // record file, the HEAD advance).
        let payload = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
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
            tick: 102,
            content_refs: vec![payload.hash()],
            depends_on: Vec::new(),
        };
        let malformed_id = crate::hash::hash_object(&malformed.canonical_bytes());
        std::fs::write(medium.object_path_pub(&payload.hash()), payload.bytes()).unwrap();
        std::fs::write(
            medium.object_path_pub(&malformed_id),
            malformed.canonical_bytes(),
        )
        .unwrap();
        let head_bytes = {
            let mut enc = engine_core::Encoder::new();
            enc.write_str(HEAD_DOMAIN);
            enc.write_u32(HEAD_VERSION);
            enc.write_bytes(malformed_id.as_bytes());
            enc.into_bytes()
        };
        std::fs::write(root.join(HEAD_FILE), head_bytes).unwrap();
        let error = medium.begin_reclamation(&audit, &mk, 60).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "epoch-regression"
                }
            ),
            "the malformed revision above the audit refuses at the authoritative-chain validation: {error:?}"
        );
        assert_eq!(
            medium.read_head_file().unwrap().unwrap(),
            malformed_id,
            "the HEAD is unchanged (nothing installed; the raw stored head)"
        );
        // (b) No duplicate success from an invalid authoritative
        // chain: a VALID same-key begin installed first (via the
        // in-crate receipt — it is coherent), then the malformed
        // revision above it — the admission refuses at the chain
        // validation, never returns the duplicate.
        let (root_b, mut medium_b, audit_b, mk_b, _shared_b, _private_b) =
            trusted_execution_store();
        let key_b = crate::reclaim::reclamation_begin_idempotency_key(&mk_b);
        let begin_b = crate::reclaim::ReclamationBegin {
            mk: mk_b,
            source: audit_b,
            range_digest: [0u8; 32],
            seed: [0u8; 32],
        };
        let object_b = StoredObject::new(begin_b.canonical_bytes()).unwrap();
        let head_b = medium_b.head().unwrap().unwrap();
        let epoch_b = medium_b.read_record(&head_b).unwrap().epoch;
        let valid = RevisionRecord::regular(head_b, epoch_b, 103)
            .with_idempotency_key(key_b)
            .with_content_refs(vec![object_b.hash()]);
        let admitted =
            crate::medium::AdmittedAppend::generic(&valid, std::slice::from_ref(&object_b));
        medium_b.append_admitted(admitted, Some(head_b)).unwrap();
        let payload_b = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"malformed-above-the-begin");
            enc.into_bytes()
        })
        .unwrap();
        let head_m = medium_b.head().unwrap().unwrap();
        let epoch_m = medium_b.read_record(&head_m).unwrap().epoch;
        let malformed_b = RevisionRecord {
            kind: crate::record::RecordKind::Regular,
            epoch: epoch_m + 5,
            parent: Some(head_m),
            idempotency_key: None,
            tick: 104,
            content_refs: vec![payload_b.hash()],
            depends_on: Vec::new(),
        };
        let malformed_b_id = crate::hash::hash_object(&malformed_b.canonical_bytes());
        std::fs::write(
            medium_b.object_path_pub(&payload_b.hash()),
            payload_b.bytes(),
        )
        .unwrap();
        std::fs::write(
            medium_b.object_path_pub(&malformed_b_id),
            malformed_b.canonical_bytes(),
        )
        .unwrap();
        let head_bytes_b = {
            let mut enc = engine_core::Encoder::new();
            enc.write_str(HEAD_DOMAIN);
            enc.write_u32(HEAD_VERSION);
            enc.write_bytes(malformed_b_id.as_bytes());
            enc.into_bytes()
        };
        std::fs::write(root_b.join(HEAD_FILE), head_bytes_b).unwrap();
        let error = medium_b.begin_reclamation(&audit_b, &mk_b, 61).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "epoch-regression"
                }
            ),
            "no duplicate success from an invalid authoritative chain: {error:?}"
        );
        // (c) The valid control: the ordinary valid-history admission
        // still succeeds.
        let (root_c, mut medium_c, audit_c, mk_c, _shared_c, _private_c) =
            trusted_execution_store();
        let outcome = medium_c.begin_reclamation(&audit_c, &mk_c, 62).unwrap();
        assert!(
            matches!(
                outcome,
                crate::reclaim::BeginPublicationOutcome::Accepted {
                    tier: crate::medium::SyncTier::FullPosixSequence,
                    ..
                }
            ),
            "the ordinary valid-history admission still succeeds: {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root_b);
        let _ = std::fs::remove_dir_all(&root_c);
    }

    #[test]
    fn the_local_begin_discovery_linkage_refuses_epoch_overflow() {
        // (C2-R7) The local overflow twin: a same-key record at epoch
        // u64::MAX over a parent at epoch u64::MAX — the checked
        // linkage refuses (saturation would have made the incoherent
        // successor appear coherent). Exercised through the
        // discovery directly (the discovery is pure storage logic —
        // no tier dependency; the authoritative-chain validation
        // refuses such a HEAD at the public admission boundary
        // first).
        let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
        let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
        let begin = crate::reclaim::ReclamationBegin {
            mk,
            source: audit,
            range_digest: [0u8; 32],
            seed: [0u8; 32],
        };
        let object = StoredObject::new(begin.canonical_bytes()).unwrap();
        let payload = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
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
            tick: 105,
            content_refs: vec![payload.hash()],
            depends_on: Vec::new(),
        };
        let parent_id = crate::hash::hash_object(&parent_record.canonical_bytes());
        let keyed = RevisionRecord {
            kind: crate::record::RecordKind::Regular,
            epoch: u64::MAX,
            parent: Some(parent_id),
            idempotency_key: Some(key),
            tick: 106,
            content_refs: vec![object.hash()],
            depends_on: Vec::new(),
        };
        let keyed_id = crate::hash::hash_object(&keyed.canonical_bytes());
        std::fs::write(medium.object_path_pub(&payload.hash()), payload.bytes()).unwrap();
        std::fs::write(medium.object_path_pub(&object.hash()), object.bytes()).unwrap();
        std::fs::write(
            medium.object_path_pub(&parent_id),
            parent_record.canonical_bytes(),
        )
        .unwrap();
        std::fs::write(medium.object_path_pub(&keyed_id), keyed.canonical_bytes()).unwrap();
        let head_bytes = {
            let mut enc = engine_core::Encoder::new();
            enc.write_str(HEAD_DOMAIN);
            enc.write_u32(HEAD_VERSION);
            enc.write_bytes(keyed_id.as_bytes());
            enc.into_bytes()
        };
        std::fs::write(root.join(HEAD_FILE), head_bytes).unwrap();
        let error = medium.find_installed_begin_locked(&mk).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "begin-admission/installed-linkage-mismatch"
                }
            ),
            "the epoch overflow refuses the linkage (u64::MAX has no coherent successor): {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_shared_begin_verifier_matrix() {
        // (OC-C3 CP-1) The shared installed-begin verifier's
        // adversarial matrix through the GENERIC adapter (the
        // completion-path borrowing model) over a local store: the
        // valid discovery and bindings; each payload field
        // individually mismatched; the wrong idempotency key; the
        // orphaned (off-chain) begin; the below-audit position; the
        // broken parent/epoch linkage; the missing and corrupt
        // parent; and the recovery-safe verification after the
        // original retirement records are gone (the authenticated
        // gap crossing included).
        // (R3) The platform split: the REAL-ADMISSION valid case is
        // POSIX-only (on a ProcessCrashOnly platform the admission
        // refuses typed — asserted); the shared verifier's own
        // adversarial matrix below is PLATFORM-NEUTRAL and runs in
        // full on every platform (the hand-installed fixtures never
        // claim trusted-admission success — the valid-LOOKING case
        // proves the recognition, and the authority's tier refusal
        // is separately proven in the authority test).
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let posix = medium.durability_tier() == crate::medium::SyncTier::FullPosixSequence;
            if !posix {
                let error = medium.begin_reclamation(&audit, &mk, 120).unwrap_err();
                assert!(matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/tier-insufficient"
                    }
                ));
            } else {
                // (a) The valid case: the real admission, then the
                // verifier through the generic adapter.
                medium.begin_reclamation(&audit, &mk, 120).unwrap();
                let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
                let head = medium.read_head_file().unwrap().unwrap();
                let mut reads =
                    crate::reclaim::MediumReads::new(&mut medium as &mut dyn CanonicalMedium);
                let found =
                    crate::reclaim::verify_installed_begin(&mut reads, head, &audit, &mk, &ledger)
                        .unwrap();
                let (id, begin) = found.expect("the admitted begin verifies");
                assert_eq!(begin.mk, mk);
                assert_eq!(begin.source, audit);
                assert_eq!(begin.range_digest, ledger.range_digest);
                assert_eq!(begin.seed, ledger.seed);
                let _ = id;
            }
            let _ = std::fs::remove_dir_all(&root);
        }
        // The hand-installed adversarial fixtures (the corruption-
        // shaped install — the R4-established pattern).
        let hand_install = |medium: &mut LocalFileMedium,
                            begin: crate::reclaim::ReclamationBegin,
                            key: Vec<u8>,
                            epoch_override: Option<u64>|
         -> Hash {
            let head = medium.read_head_file().unwrap().unwrap();
            let head_epoch = medium.read_record(&head).unwrap().epoch;
            let object = StoredObject::new(begin.canonical_bytes()).unwrap();
            let record = RevisionRecord {
                kind: crate::record::RecordKind::Regular,
                epoch: epoch_override.unwrap_or(head_epoch + 1),
                parent: Some(head),
                idempotency_key: Some(key),
                tick: 130,
                content_refs: vec![object.hash()],
                depends_on: Vec::new(),
            };
            let record_id = crate::hash::hash_object(&record.canonical_bytes());
            std::fs::write(medium.object_path_pub(&object.hash()), object.bytes()).unwrap();
            std::fs::write(medium.object_path_pub(&record_id), record.canonical_bytes()).unwrap();
            let head_bytes = {
                let mut enc = engine_core::Encoder::new();
                enc.write_str(HEAD_DOMAIN);
                enc.write_u32(HEAD_VERSION);
                enc.write_bytes(record_id.as_bytes());
                enc.into_bytes()
            };
            std::fs::write(medium.root().join(HEAD_FILE), head_bytes).unwrap();
            record_id
        };
        // (a2, R3) The platform-neutral valid-LOOKING case: a
        // hand-installed begin with the CORRECT bindings — the
        // shared verifier RECOGNIZES it on EVERY platform (its
        // checks are storage logic, tier-independent), including
        // Windows where the destructive grade remains refused by
        // the authority's platform tier gate (proven in the
        // authority test). The fixture is explicitly adversarial —
        // never a trusted-admission success.
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let fixture = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            hand_install(&mut medium, fixture, key, None);
            let head = medium.read_head_file().unwrap().unwrap();
            let mut reads =
                crate::reclaim::MediumReads::new(&mut medium as &mut dyn CanonicalMedium);
            let found =
                crate::reclaim::verify_installed_begin(&mut reads, head, &audit, &mk, &ledger)
                    .unwrap();
            assert!(
                found.is_some(),
                "the verifier's recognition is platform-neutral (the grade policy is not)"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) Each payload field individually mismatched.
        for label in ["wrong mk", "wrong source", "wrong digest", "wrong seed"] {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            // The fixture differs from the AUTHENTIC ledger in
            // exactly one field per case.
            let mut fixture = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            match label {
                // A REAL non-zero identity (the strict decode refuses
                // a zero mk before the binding check could fire).
                "wrong mk" => fixture.mk = ledger.sk,
                "wrong source" => fixture.source = ledger.sk,
                "wrong digest" => fixture.range_digest = [1u8; 32],
                _ => fixture.seed = [2u8; 32],
            }
            hand_install(&mut medium, fixture, key, None);
            let head = medium.read_head_file().unwrap().unwrap();
            let mut reads =
                crate::reclaim::MediumReads::new(&mut medium as &mut dyn CanonicalMedium);
            let error =
                crate::reclaim::verify_installed_begin(&mut reads, head, &audit, &mk, &ledger)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-authority/binding-mismatch"
                    }
                ),
                "{label} refuses at the binding: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (c) The wrong idempotency key (a keyed record that is not
        // the MK-bound begin key — the walk passes it, finding no
        // begin: the DEFINITE absence).
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
            let fixture = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            hand_install(&mut medium, fixture, b"not-the-begin-key".to_vec(), None);
            let head = medium.read_head_file().unwrap().unwrap();
            let mut reads =
                crate::reclaim::MediumReads::new(&mut medium as &mut dyn CanonicalMedium);
            let found =
                crate::reclaim::verify_installed_begin(&mut reads, head, &audit, &mk, &ledger)
                    .unwrap();
            assert!(
                found.is_none(),
                "a non-begin-keyed record is a definite canonical absence"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (d) The broken parent/epoch linkage (the hand-installed
        // begin at an incoherent epoch — the C2-R7 checked
        // arithmetic refuses).
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let fixture = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            hand_install(&mut medium, fixture, key, Some(u64::MAX));
            let head = medium.read_head_file().unwrap().unwrap();
            let mut reads =
                crate::reclaim::MediumReads::new(&mut medium as &mut dyn CanonicalMedium);
            let error =
                crate::reclaim::verify_installed_begin(&mut reads, head, &audit, &mk, &ledger)
                    .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-authority/installed-linkage-mismatch"
                    }
                ),
                "the incoherent linkage refuses: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (e) The orphaned (off-chain) begin: hand-written into the
        // object namespace, never on the ancestry — a definite
        // absence (never authority).
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let fixture = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let object = StoredObject::new(fixture.canonical_bytes()).unwrap();
            let orphan = RevisionRecord::regular(Hash::ZERO, 0, 131)
                .with_idempotency_key(key)
                .with_content_refs(vec![object.hash()]);
            std::fs::write(medium.object_path_pub(&object.hash()), object.bytes()).unwrap();
            std::fs::write(
                medium.object_path_pub(&orphan.id()),
                orphan.canonical_bytes(),
            )
            .unwrap();
            let head = medium.read_head_file().unwrap().unwrap();
            let mut reads =
                crate::reclaim::MediumReads::new(&mut medium as &mut dyn CanonicalMedium);
            let found =
                crate::reclaim::verify_installed_begin(&mut reads, head, &audit, &mk, &ledger)
                    .unwrap();
            assert!(found.is_none(), "the orphan never verifies");
            let _ = std::fs::remove_dir_all(&root);
        }
        // (f, R3) The corrupt parent — platform-neutral: the
        // hand-installed begin with CORRECT bindings, its parent
        // (the prior head — the audit) corrupted — the typed
        // integrity refusal at the hash-verifying read.
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
            let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
            let fixture = crate::reclaim::ReclamationBegin {
                mk,
                source: audit,
                range_digest: ledger.range_digest,
                seed: ledger.seed,
            };
            let begin_head = hand_install(&mut medium, fixture, key, None);
            // The corrupt parent: overwrite the begin's PARENT (the
            // prior head — the audit) with wrong bytes at the same
            // path.
            let parent = medium
                .read_record(&begin_head)
                .unwrap()
                .parent
                .expect("the begin has a parent");
            std::fs::write(medium.object_path_pub(&parent), b"corrupted-parent-record").unwrap();
            let mut reads =
                crate::reclaim::MediumReads::new(&mut medium as &mut dyn CanonicalMedium);
            let error = crate::reclaim::verify_installed_begin(
                &mut reads, begin_head, &audit, &mk, &ledger,
            )
            .unwrap_err();
            assert!(
                matches!(error, MediumError::HashMismatch { .. }),
                "the corrupt parent refuses at the hash-verifying read: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (g) The recovery-safe case with the original retirement
        // records deleted and an authenticated gap crossed: the
        // two-retirement fixture, A's retirement executed, B's
        // begin installed — the verifier needs none of A's deleted
        // range.
        {
            let (root, mut medium, audit_a, mk1, _mk2, _x1, _x2, directive_a, _ledger_b) =
                two_retirement_store("cp1-verifier-recovery");
            if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
            medium.reclaim(&directive_a).unwrap();
            // B's audit published above A's retired range, then B's
            // begin.
            let (_audit_b, outcome) =
                publish_reclamation_ledger(&mut medium, &_ledger_b, 9).unwrap();
            assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
            let ledger_b = _ledger_b;
            let _head = medium.read_head_file().unwrap().unwrap();
            // B's begin admission over the compacted history.
            let begin_outcome = medium
                .begin_reclamation(&_audit_b, &ledger_b.mk, 133)
                .unwrap();
            assert!(
                matches!(
                    begin_outcome,
                    crate::reclaim::BeginPublicationOutcome::Accepted { .. }
                ),
                "the compacted-history admission accepts: {begin_outcome:?}"
            );
            let begin_head = medium.read_head_file().unwrap().unwrap();
            let mut reads =
                crate::reclaim::MediumReads::new(&mut medium as &mut dyn CanonicalMedium);
            let found = crate::reclaim::verify_installed_begin(
                &mut reads,
                begin_head,
                &_audit_b,
                &ledger_b.mk,
                &ledger_b,
            )
            .unwrap();
            assert!(
                found.is_some(),
                "the recovery-safe verifier crosses the gap and needs no deleted record"
            );
            let _ = (audit_a, mk1);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_local_begin_authority_reaffirmation() {
        // (OC-C3 CP-1) The installed-begin durability authority on
        // the local provider: the tier-gated platform refusal (a
        // hand-installed begin must NOT override ProcessCrashOnly);
        // the POSIX grade established only through the completed
        // reaffirmation (the accepted begin, the begin below HEAD
        // with intervening revisions, the previously-installed
        // begin); the interrupted synchronization (the fault seam —
        // never success); and the unreadable authorization
        // dependency.
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            // (OC-P2) The platform tier gate: on ProcessCrashOnly,
            // even a fully valid hand-installed begin cannot obtain
            // the authority — the restored/discoverable begin must
            // not override the platform limitation.
            if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
                let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
                let key = crate::reclaim::reclamation_begin_idempotency_key(&mk);
                let fixture = crate::reclaim::ReclamationBegin {
                    mk,
                    source: audit,
                    range_digest: ledger.range_digest,
                    seed: ledger.seed,
                };
                let object = StoredObject::new(fixture.canonical_bytes()).unwrap();
                let head = medium.read_head_file().unwrap().unwrap();
                let head_epoch = medium.read_record(&head).unwrap().epoch;
                let record = RevisionRecord::regular(head, head_epoch, 140)
                    .with_idempotency_key(key)
                    .with_content_refs(vec![object.hash()]);
                let record_id = crate::hash::hash_object(&record.canonical_bytes());
                std::fs::write(medium.object_path_pub(&object.hash()), object.bytes()).unwrap();
                std::fs::write(medium.object_path_pub(&record_id), record.canonical_bytes())
                    .unwrap();
                let head_bytes = {
                    let mut enc = engine_core::Encoder::new();
                    enc.write_str(HEAD_DOMAIN);
                    enc.write_u32(HEAD_VERSION);
                    enc.write_bytes(record_id.as_bytes());
                    enc.into_bytes()
                };
                std::fs::write(medium.root().join(HEAD_FILE), head_bytes).unwrap();
                let error = medium.verify_begin_authority(&audit, &mk).unwrap_err();
                assert!(
                    matches!(
                        error,
                        MediumError::Chain {
                            rule: "begin-authority/tier-insufficient"
                        }
                    ),
                    "the discoverable begin never overrides the platform tier: {error:?}"
                );
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
            // (a) The accepted begin: the authority established at
            // FullPosixSequence through the completed reaffirmation.
            let outcome = medium.begin_reclamation(&audit, &mk, 141).unwrap();
            let installed = match outcome {
                crate::reclaim::BeginPublicationOutcome::Accepted { id, .. } => id,
                other => panic!("the admission accepts: {other:?}"),
            };
            let authority = medium.verify_begin_authority(&audit, &mk).unwrap();
            assert_eq!(authority.id, installed);
            assert_eq!(authority.tier, crate::medium::SyncTier::FullPosixSequence);
            // (b) The begin below HEAD with intervening canonical
            // revisions: append records above the begin, then the
            // authority still verifies (the walked path covers the
            // intervening records).
            for tick in 142..=144u64 {
                let head = medium.head().unwrap().unwrap();
                let head_epoch = medium.read_record(&head).unwrap().epoch;
                let payload = StoredObject::new({
                    let mut enc = engine_core::Encoder::new();
                    enc.write_str("sektor7/save/1");
                    enc.write_bytes(b"intervening-revision");
                    enc.into_bytes()
                })
                .unwrap();
                let record = RevisionRecord::regular(head, head_epoch, tick)
                    .with_content_refs(vec![payload.hash()]);
                medium
                    .append(&record, std::slice::from_ref(&payload), Some(head))
                    .unwrap();
            }
            let authority = medium.verify_begin_authority(&audit, &mk).unwrap();
            assert_eq!(authority.id, installed);
            assert_eq!(authority.tier, crate::medium::SyncTier::FullPosixSequence);
            // (c) The interrupted synchronization: the fault seam
            // fires after the file syncs, before the directory
            // anchoring — the typed I/O refusal, NEVER success.
            medium.fault = Some(FaultControl::new().arm(
                FaultPoint::ReaffirmFilesSynced,
                crate::fault::FaultAction::InjectError,
            ));
            let error = medium.verify_begin_authority(&audit, &mk).unwrap_err();
            assert!(
                matches!(error, MediumError::Io { .. }),
                "the interrupted reaffirmation never returns success: {error:?}"
            );
            // The armed seam persists (non-consuming): a repeat call
            // refuses identically — never success.
            let error = medium.verify_begin_authority(&audit, &mk).unwrap_err();
            assert!(
                matches!(error, MediumError::Io { .. }),
                "the armed seam refuses every attempt: {error:?}"
            );
            medium.fault = None;
            // (d) The absent begin (a fresh store) — the typed
            // absence.
            let (root2, mut medium2, audit2, mk2, _s2, _p2) = trusted_execution_store();
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
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&root2);
        }
    }
    #[test]
    fn the_same_retirement_begin_recovery_after_deletion() {
        // (OC-C3 CP-1 corrective R3) The SAME-retirement recovery:
        // the begin remains discoverable — and its authority
        // recoverable — after the VERY retirement records it
        // authorized are deleted (the authenticated skip crossing
        // over the retirement's own gap). Two variants: the FULL
        // deletion (the legacy pre-CP-2 reclamation executes the
        // whole retirement) and the PARTIAL PREFIX (the interrupted
        // PASS 1 shape — the first unit deleted, the second
        // present). The medium is REOPENED for the recovery (no
        // in-memory authorization state); no replacement begin is
        // published; the HEAD is not advanced by the authority
        // verification. The POSIX-only scenario (the real admission
        // and the FullPosixSequence reaffirmation).
        // (a) The full deletion.
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            // (OC-P2) The tier gate: the admission (and the
            // reaffirmation grade) is POSIX-only.
            if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
                let error = medium.begin_reclamation(&audit, &mk, 180).unwrap_err();
                assert!(matches!(
                    error,
                    MediumError::Chain {
                        rule: "begin-admission/tier-insufficient"
                    }
                ));
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
            // The real begin through the accepted admission, then
            // the pre-deletion authority.
            let outcome = medium.begin_reclamation(&audit, &mk, 180).unwrap();
            let installed = match outcome {
                crate::reclaim::BeginPublicationOutcome::Accepted { id, .. } => id,
                other => panic!("the admission accepts: {other:?}"),
            };
            let authority = medium.verify_begin_authority(&audit, &mk).unwrap();
            assert_eq!(authority.id(), installed);
            assert_eq!(authority.tier(), crate::medium::SyncTier::FullPosixSequence);
            // The legacy (pre-CP-2) reclamation executes the
            // retirement — deleting the very records the begin
            // authorized (permitted for constructing the isolated
            // test state: CP-1 does not yet enforce Option C; the
            // implementation is unmodified).
            let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
            medium
                .reclaim(&crate::reclaim::ReclaimDirective {
                    audit_record: audit,
                    units: ledger.units.clone(),
                })
                .unwrap();
            // The original retirement records are absent.
            for unit in &ledger.units {
                assert!(
                    !medium.object_path_pub(&unit.record).is_file(),
                    "the retirement record {} is deleted",
                    unit.record
                );
            }
            // REOPEN the medium — the recovery without any
            // in-memory authorization state.
            drop(medium);
            let mut reopened = LocalFileMedium::open(&root).unwrap();
            let authority = reopened.verify_begin_authority(&audit, &mk).unwrap();
            assert_eq!(
                authority.id(),
                installed,
                "the ORIGINAL installed begin identity is recovered"
            );
            assert_eq!(
                authority.tier(),
                crate::medium::SyncTier::FullPosixSequence,
                "the grade re-established only after the completed reaffirmation"
            );
            // No replacement begin was published and the HEAD was
            // not advanced by the authority verification (the begin
            // remains the HEAD — the reclamation deletes only).
            assert_eq!(
                reopened.read_head_file().unwrap().unwrap(),
                installed,
                "the begin remains the HEAD; the authority advanced nothing"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) The partial deletion prefix (the interrupted PASS 1
        // shape: the FIRST — newest — unit's record deleted, the
        // second still present).
        {
            let (root, mut medium, audit, mk, _shared, _private) = trusted_execution_store();
            if medium.durability_tier() != crate::medium::SyncTier::FullPosixSequence {
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
            let outcome = medium.begin_reclamation(&audit, &mk, 181).unwrap();
            let installed = match outcome {
                crate::reclaim::BeginPublicationOutcome::Accepted { id, .. } => id,
                other => panic!("the admission accepts: {other:?}"),
            };
            let (ledger, _metadata, _head) = medium.authenticate_audit_locked(&audit).unwrap();
            // The genuine prefix shape: only the FIRST unit's record
            // removed (the newest-first order — the interrupted
            // deletion after one step), the second unit intact.
            std::fs::remove_file(medium.object_path_pub(&ledger.units[0].record)).unwrap();
            assert!(
                medium.object_path_pub(&ledger.units[1].record).is_file(),
                "the second unit remains — the prefix shape"
            );
            drop(medium);
            let mut reopened = LocalFileMedium::open(&root).unwrap();
            let authority = reopened.verify_begin_authority(&audit, &mk).unwrap();
            assert_eq!(authority.id(), installed);
            assert_eq!(authority.tier(), crate::medium::SyncTier::FullPosixSequence);
            assert_eq!(
                reopened.read_head_file().unwrap().unwrap(),
                installed,
                "the begin remains the HEAD"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
