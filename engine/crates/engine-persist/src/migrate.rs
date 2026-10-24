//! §6.1's operator-controlled offline migration (PP-B1): converting
//! a legacy [`LocalFileMedium`] campaign store into a SQLite campaign
//! database — copy, verify, seal, activate, archive — with the
//! mandatory directory-layout safety check before anything is
//! touched and the fail-closed rules the ratified design binds:
//!
//! * **Offline, operator-controlled, never automatic.** The tool is
//!   invoked deliberately between sessions; nothing here runs at
//!   session startup, and the quiescence (no live session on either
//!   side) is the operator's contract.
//! * **Idempotent and restartable at every interruption point.** A
//!   re-run resumes: an interrupted copy appends only the records
//!   the target still lacks (each with its own parent as the
//!   expected parent); a post-seal interruption completes the
//!   activation and the archive; a completed migration reports
//!   itself without touching anything.
//! * **Never merges divergent histories.** A target whose head is
//!   not on the source chain is refused; a source that moved between
//!   the copy and the cutover is refused at the raw recheck.
//! * **Seal before activate.** The legacy directory receives the
//!   `MIGRATED` tombstone (create-new; new binaries refuse to open
//!   it — older binaries cannot be made to honor it, the ratified
//!   documented operational limitation) before the database's
//!   provenance row is written; the activation happens only over a
//!   verified seal.
//! * **The archive never takes the new database or the live
//!   journal** — the mandatory layout check proves it first.
//!
//! The T1 journal is untouched (the ratified D-PP-2): it stays
//! beside the new database, and the standard recovery composes the
//! migrated chain with it exactly as before.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use engine_core::{Decoder, Encoder};

use crate::hash::Hash;
use crate::local::{HEAD_LOCK, LocalFileMedium, MIGRATED_MARKER, read_head_at};
use crate::medium::{AppendOutcome, CanonicalMedium, ChainSummary, MediumError, StoredObject};
use crate::record::RevisionRecord;
use crate::sqlite::SqliteMedium;

/// The `MIGRATED` tombstone's wire domain (versioned and
/// self-describing — the `head/1` precedent).
pub const MIGRATED_DOMAIN: &str = "sektor7/engine-persist/migrated/1";
const MIGRATED_VERSION: u32 = 1;

/// The migration's typed failures — fail-closed everywhere: the
/// source is never sealed on a failure path, the target is never
/// activated, and no partial state is ever silently promoted.
#[derive(Debug)]
pub enum MigrationError {
    /// The mandatory directory-layout safety check failed: the
    /// archive could take the new database or the live journal, or
    /// the pair lives inside the legacy store directory. Nothing was
    /// touched.
    Layout { detail: String },
    /// The source and the target do not describe the same history (a
    /// foreign target chain, a parity failure, or a source that moved
    /// between the copy and the cutover). Divergent histories are
    /// never merged.
    Diverged { detail: String },
    /// A copy append's outcome was `Unknown` — the disposition is
    /// genuinely uncertain. Re-running is safe and correct: the
    /// resume logic tolerates either disposition (an installed
    /// record is skipped by the head, an uninstalled one is
    /// re-appended with the same identity).
    Uncertain { detail: String },
    /// The `MIGRATED` tombstone is present but unparseable — the
    /// seal's evidence is corrupt; nothing is guessed.
    TombstoneCorrupt { detail: String },
    /// A medium-level failure, carried through.
    Medium(MediumError),
    /// Filesystem I/O during the migration's own file operations.
    Io {
        op: &'static str,
        path: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::Layout { detail } => {
                write!(f, "the directory layout is unsafe to migrate: {detail}")
            }
            MigrationError::Diverged { detail } => {
                write!(f, "the migration would merge divergent histories: {detail}")
            }
            MigrationError::Uncertain { detail } => {
                write!(f, "an uncertain copy outcome: {detail}")
            }
            MigrationError::TombstoneCorrupt { detail } => {
                write!(f, "the MIGRATED tombstone is corrupt: {detail}")
            }
            MigrationError::Medium(error) => write!(f, "medium failure: {error}"),
            MigrationError::Io { op, path, source } => {
                write!(f, "migration I/O failure during {op} at {path}: {source}")
            }
        }
    }
}

impl std::error::Error for MigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MigrationError::Medium(error) => Some(error),
            MigrationError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// What a completed (or resumed-to-completion) migration established.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    /// The legacy chain's summary as validated this run (`None` when
    /// resuming a sealed or already-archived source — the seal froze
    /// it before this run began; the target's own validation is the
    /// evidence).
    pub source_summary: Option<ChainSummary>,
    /// The target database's post-migration validated summary.
    pub target_summary: ChainSummary,
    /// Records this run appended (0 for an idempotent resume).
    pub copied: u64,
    /// Where the sealed legacy directory now rests (`None` when the
    /// archive already happened in a prior run).
    pub archived_to: Option<PathBuf>,
    /// True when this run resumed a previously-sealed or fully
    /// completed migration rather than performing a fresh copy.
    pub resumed: bool,
}

impl MigrationReport {
    /// §6.1's final step — the operator's verified checklist. The
    /// tool proves the chain parity and the seal; the gameplay
    /// eligibility is the operator's own verification, never the
    /// tool's claim.
    pub fn operator_checklist(&self) -> Vec<String> {
        vec![
            "The tool proved the database chain identical to the sealed source (the ChainSummary parity); re-run any read-only audit yourself for independent evidence.".to_owned(),
            "Open the campaign through the SQLite provider and confirm the recovered session: the expected region levels, the milestone and credit totals, the journaled boundaries replaying.".to_owned(),
            "Only after that verification is the migrated campaign gameplay-eligible (the ratified D-PP-6 rule).".to_owned(),
            "The archived directory is evidence: never deleted, never re-opened as a source by new binaries. Pre-tombstone binaries can still open it — the documented limitation; keep them away from the archive, or rely on its non-standard path.".to_owned(),
        ]
    }
}

/// What the copy phase established.
pub struct CopiedChain {
    /// The source's validated summary (the parity's left side).
    pub source_summary: ChainSummary,
    /// The target's post-copy validated summary (the parity's right
    /// side — equal, or the copy failed closed).
    pub target_summary: ChainSummary,
    /// Records this call appended.
    pub appended: u64,
}

/// The lexically absolute form (no filesystem access — the target and
/// journal may not exist yet); consistent for comparison.
fn absolute_path(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The mandatory directory-layout safety check: before anything is
/// touched, prove the archive cannot take the new database or the
/// live journal — neither may live inside the legacy store directory
/// (which the archive renames away) nor inside the archive's future
/// name family, and the two must be distinct paths.
fn check_layout(
    legacy_root: &Path,
    target_db: &Path,
    journal_path: &Path,
) -> Result<(), MigrationError> {
    let legacy = absolute_path(legacy_root);
    let target = absolute_path(target_db);
    let journal = absolute_path(journal_path);
    if target == journal {
        return Err(MigrationError::Layout {
            detail: "the target database and the journal are the same path".to_owned(),
        });
    }
    for (path, what) in [
        (&target, "the target database"),
        (&journal, "the live journal"),
    ] {
        if path.starts_with(&legacy) {
            return Err(MigrationError::Layout {
                detail: format!(
                    "{what} lives inside the legacy store directory {}",
                    legacy.display()
                ),
            });
        }
        let archive_prefix = format!("{}.migrated-", legacy.to_string_lossy());
        if path.to_string_lossy().starts_with(&archive_prefix) {
            return Err(MigrationError::Layout {
                detail: format!(
                    "{what} lives inside the archive's name family ({archive_prefix}*)"
                ),
            });
        }
        // The symlink/alias case: re-check through canonicalization
        // when both paths exist.
        if let (Ok(l), Ok(p)) = (legacy_root.canonicalize(), path.canonicalize())
            && (p == l || p.starts_with(&l))
        {
            return Err(MigrationError::Layout {
                detail: format!("{what} resolves inside the legacy store directory (a symlink)"),
            });
        }
    }
    Ok(())
}

/// The `MIGRATED` tombstone's canonical bytes: the migrated domain,
/// its version, and the sealed chain's head.
fn tombstone_bytes(head: &Hash) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(MIGRATED_DOMAIN);
    enc.write_u32(MIGRATED_VERSION);
    enc.write_bytes(head.as_bytes());
    enc.into_bytes()
}

/// Decodes the tombstone's bound head (domain and version verified;
/// trailing bytes fail closed).
fn decode_tombstone(bytes: &[u8]) -> Result<Hash, MigrationError> {
    let corrupt = |detail: String| MigrationError::TombstoneCorrupt { detail };
    let mut dec = Decoder::new(bytes);
    dec.read_domain(MIGRATED_DOMAIN)
        .map_err(|e| corrupt(e.to_string()))?;
    let version = dec.read_u32().map_err(|e| corrupt(e.to_string()))?;
    if version != MIGRATED_VERSION {
        return Err(corrupt(format!("unsupported tombstone version {version}")));
    }
    let head_bytes = dec
        .read_bytes(Hash::WIDTH, "migrated-head")
        .map_err(|e| corrupt(e.to_string()))?;
    let head = Hash::from_bytes(head_bytes).ok_or_else(|| corrupt("hash width".to_owned()))?;
    dec.finish().map_err(|e| corrupt(e.to_string()))?;
    Ok(head)
}

/// Writes the seal: the `MIGRATED` tombstone, create-new (an existing
/// tombstone is a prior seal — the caller checks first), fsynced,
/// with the store root's directory sync attempted best-effort (the
/// same Windows caveat as every directory sync).
fn write_tombstone(root: &Path, head: &Hash) -> Result<(), MigrationError> {
    let path = root.join(MIGRATED_MARKER);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| io_err("create MIGRATED", &path, e))?;
    file.write_all(&tombstone_bytes(head))
        .map_err(|e| io_err("write MIGRATED", &path, e))?;
    file.sync_all()
        .map_err(|e| io_err("sync MIGRATED", &path, e))?;
    drop(file);
    let _ = File::open(root).and_then(|dir| dir.sync_all());
    Ok(())
}

/// §6.1's copy: validates the legacy source cold (every record, every
/// referenced object), appends only the records the target still
/// lacks — oldest first, each with its own parent as the expected
/// parent (the resume rule: an interrupted copy is detected by the
/// target's current head and continued from there, never restarted,
/// never merged with a foreign chain) — then proves the parity (the
/// complete `ChainSummary` equality). Idempotent: a complete target
/// appends nothing and just re-proves the parity. **The target must
/// be a migration-mode instance** (`SqliteMedium::open_migration_target`):
/// the pending-target lifecycle is enforced at the open gates and
/// the activation, never inside the copy itself.
pub fn copy_legacy_chain(
    legacy: &mut LocalFileMedium,
    target: &mut SqliteMedium,
) -> Result<CopiedChain, MigrationError> {
    let source_summary = legacy.validate().map_err(MigrationError::Medium)?;
    let db_head = target.head().map_err(MigrationError::Medium)?;
    // The suffix to append: the legacy records strictly newer than
    // the target's current head, collected newest-first then applied
    // oldest-first.
    let mut suffix: Vec<(RevisionRecord, Vec<StoredObject>)> = Vec::new();
    let mut cursor = Some(source_summary.head);
    while cursor != db_head {
        let id = match cursor {
            Some(id) => id,
            // Walked past the founding without finding the target's
            // head: the target holds a chain this source does not —
            // divergent histories are never merged.
            None => {
                return Err(MigrationError::Diverged {
                    detail: format!(
                        "the target database's head is not on the source chain \
                         (the source holds {} records)",
                        source_summary.records
                    ),
                });
            }
        };
        let record = legacy.read_record(&id).map_err(MigrationError::Medium)?;
        let objects = record
            .content_refs
            .iter()
            .map(|hash| {
                let bytes = legacy.read_object(hash).map_err(MigrationError::Medium)?;
                StoredObject::new(bytes).map_err(MigrationError::Medium)
            })
            .collect::<Result<Vec<_>, _>>()?;
        cursor = record.parent;
        suffix.push((record, objects));
    }
    suffix.reverse();
    let mut appended = 0u64;
    for (record, objects) in &suffix {
        let outcome = target
            .append(record, objects, record.parent)
            .map_err(MigrationError::Medium)?;
        match outcome {
            AppendOutcome::Accepted { .. } => appended += 1,
            // The expected parent is the record's own parent — the
            // current head by construction; a rejection means
            // something else moved the target: fail closed.
            AppendOutcome::RejectedStaleParent { observed } => {
                let expected = record
                    .parent
                    .map(|hash| hash.to_string())
                    .unwrap_or_else(|| "unfounded".to_owned());
                let observed = observed
                    .map(|hash| hash.to_string())
                    .unwrap_or_else(|| "unfounded".to_owned());
                return Err(MigrationError::Diverged {
                    detail: format!(
                        "the target moved mid-copy (expected parent {expected}, \
                         observed head {observed})"
                    ),
                });
            }
            AppendOutcome::Unknown { reason } => {
                return Err(MigrationError::Uncertain {
                    detail: format!(
                        "a copy append returned Unknown ({reason:?}); re-run the \
                         migration — the resume tolerates either disposition"
                    ),
                });
            }
        }
    }
    let target_summary = target.validate().map_err(MigrationError::Medium)?;
    if target_summary != source_summary {
        return Err(MigrationError::Diverged {
            detail: format!(
                "the parity failed: the source summary {source_summary:?} does not \
                 equal the target summary {target_summary:?}"
            ),
        });
    }
    Ok(CopiedChain {
        source_summary,
        target_summary,
        appended,
    })
}

/// §6.1's cutover: acquire the source's `HEAD.lock` and hold it
/// across the recheck–seal–activate sequence — the raw, cursor-free
/// HEAD recheck (a source that moved between the copy and the
/// cutover fails closed, never merged), then the seal (the
/// `MIGRATED` tombstone, create-new), then the activation (the
/// provenance row, only over the just-verified seal). The lock is
/// released before this returns; the caller archives afterwards (the
/// rename must not race an open Windows handle).
pub fn cutover_seal_activate(
    legacy_root: &Path,
    expected_head: &Hash,
    target: &mut SqliteMedium,
) -> Result<(), MigrationError> {
    let lock_path = legacy_root.join(HEAD_LOCK);
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| io_err("open HEAD.lock", &lock_path, e))?;
    lock_file
        .lock()
        .map_err(|e| io_err("lock HEAD.lock", &lock_path, e))?;
    let outcome = (|| {
        let observed = read_head_at(legacy_root).map_err(MigrationError::Medium)?;
        if observed != Some(*expected_head) {
            let observed = observed
                .map(|hash| hash.to_string())
                .unwrap_or_else(|| "unfounded".to_owned());
            return Err(MigrationError::Diverged {
                detail: format!(
                    "the source moved between the copy and the cutover \
                     (expected {expected_head}, observed {observed})"
                ),
            });
        }
        write_tombstone(legacy_root, expected_head)?;
        target
            .mark_migrated_from(expected_head)
            .map_err(MigrationError::Medium)?;
        Ok(())
    })();
    let _ = lock_file.unlock();
    outcome
}

/// §6.1's archive: the sealed legacy directory moves out of its
/// standard discoverable path — `<name>.migrated-<nanos>/` beside it
/// (a fresh unique name per call; the directory is renamed whole,
/// tombstone and all). The layout check (performed at the entry) is
/// what makes this safe for the database and journal that stay
/// behind.
pub fn archive_legacy(legacy_root: &Path) -> Result<PathBuf, MigrationError> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| io_err("clock", legacy_root, std::io::Error::other(e)))?
        .as_nanos();
    let mut candidate = archive_path(legacy_root, stamp, 0);
    let mut n = 0u32;
    while candidate.exists() {
        n += 1;
        candidate = archive_path(legacy_root, stamp, n);
    }
    fs::rename(legacy_root, &candidate)
        .map_err(|e| io_err("archive the legacy store", legacy_root, e))?;
    Ok(candidate)
}

fn archive_path(root: &Path, stamp: u128, disambiguator: u32) -> PathBuf {
    let mut name = match root.file_name() {
        Some(name) => name.to_os_string(),
        None => return root.with_file_name(format!("migrated-{stamp}")),
    };
    name.push(format!(".migrated-{stamp}"));
    if disambiguator > 0 {
        name.push(format!("-{disambiguator}"));
    }
    root.with_file_name(name)
}

/// Whether a prior archive of this source exists (the sibling name
/// family) — the completed-migration detection.
fn find_archive(legacy_root: &Path) -> Result<Option<PathBuf>, MigrationError> {
    let prefix = match legacy_root.file_name() {
        Some(name) => format!("{}.migrated-", name.to_string_lossy()),
        None => return Ok(None),
    };
    let parent = legacy_root.parent().unwrap_or_else(|| Path::new("."));
    let entries =
        fs::read_dir(parent).map_err(|e| io_err("read the campaign directory", parent, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_err("read the campaign directory", parent, e))?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// §6.1's operator-controlled offline migration — the composed entry.
/// The mandatory layout check runs first (nothing is touched before
/// it passes); an interrupted migration resumes at whichever step it
/// reached; a completed one reports itself. The caller's contract:
/// offline (no live session on either side — no open crossing, no
/// other process holding either store).
pub fn migrate_local_to_sqlite(
    legacy_root: &Path,
    target_db: &Path,
    journal_path: &Path,
) -> Result<MigrationReport, MigrationError> {
    check_layout(legacy_root, target_db, journal_path)?;

    // The completed-migration detection: the source absent with its
    // archive present — validate the target and report; touch
    // nothing.
    if !legacy_root.exists() {
        if find_archive(legacy_root)?.is_some() {
            let mut target = SqliteMedium::open(target_db).map_err(MigrationError::Medium)?;
            let target_summary = target.validate().map_err(MigrationError::Medium)?;
            return Ok(MigrationReport {
                source_summary: None,
                target_summary,
                copied: 0,
                archived_to: None,
                resumed: true,
            });
        }
        return Err(MigrationError::Layout {
            detail: format!(
                "the legacy source {} is absent and no archive sibling exists",
                legacy_root.display()
            ),
        });
    }

    // The post-seal resume: the tombstone present (the cutover was
    // interrupted after the seal) — the source is frozen; finish the
    // activation over the verified seal and the archive.
    let tombstone_path = legacy_root.join(MIGRATED_MARKER);
    if tombstone_path.is_file() {
        let bytes =
            fs::read(&tombstone_path).map_err(|e| io_err("read MIGRATED", &tombstone_path, e))?;
        let sealed_head = decode_tombstone(&bytes)?;
        // The target's lifecycle state decides the opener (the
        // review corrective): a PENDING target (the interruption
        // between the seal and the activation) opens in migration
        // mode; an ACTIVATED one (the interruption between the
        // activation and the archive) opens normally — and the
        // idempotent activation accepts the completed binding.
        let mut target = match SqliteMedium::open(target_db) {
            Ok(target) => target,
            Err(MediumError::MigrationPending { .. }) => {
                SqliteMedium::open_migration_target(target_db).map_err(MigrationError::Medium)?
            }
            Err(other) => return Err(MigrationError::Medium(other)),
        };
        let target_summary = target.validate().map_err(MigrationError::Medium)?;
        if target_summary.head != sealed_head {
            return Err(MigrationError::Diverged {
                detail: format!(
                    "the target head {} does not match the sealed source head {sealed_head}",
                    target_summary.head
                ),
            });
        }
        // THE LEGACY RECHECK (the review corrective — Finding 2): the
        // tombstone binds the sealed head, but a pre-tombstone binary
        // may have ignored it and advanced the source after the seal.
        // Before the resume archives the source as a completed
        // migration, recheck the ACTUAL legacy HEAD under the same
        // held-lock coordination as the cutover — a changed source
        // fails closed (never auto-merged, never archived as a
        // success; the divergence is the operator's decision). The
        // lock is released before the archive rename (Windows cannot
        // rename a directory containing a locked file) — the residual
        // unlock-to-rename window is the documented older-binary
        // limitation: new binaries refuse the tombstone, and the
        // ratified caveat (no mechanically guaranteed cross-medium
        // transfer under every adversarial timing) stands.
        let lock_path = legacy_root.join(HEAD_LOCK);
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| io_err("open HEAD.lock", &lock_path, e))?;
        lock_file
            .lock()
            .map_err(|e| io_err("lock HEAD.lock", &lock_path, e))?;
        let rechecked = read_head_at(legacy_root).map_err(MigrationError::Medium)?;
        let activation = (|| {
            if rechecked != Some(sealed_head) {
                let observed = rechecked
                    .map(|hash| hash.to_string())
                    .unwrap_or_else(|| "unfounded".to_owned());
                return Err(MigrationError::Diverged {
                    detail: format!(
                        "the sealed source moved after the seal (expected \
                         {sealed_head}, observed {observed}) — an older binary may \
                         have ignored the tombstone; the divergence requires the \
                         operator's decision and the source is NOT archived"
                    ),
                });
            }
            target
                .mark_migrated_from(&sealed_head)
                .map_err(MigrationError::Medium)?;
            Ok(())
        })();
        let _ = lock_file.unlock();
        drop(lock_file);
        activation?;
        drop(target);
        let archived_to = archive_legacy(legacy_root)?;
        return Ok(MigrationReport {
            source_summary: None,
            target_summary,
            copied: 0,
            archived_to: Some(archived_to),
            resumed: true,
        });
    }

    // The fresh migration: validate and copy (idempotent), then the
    // cutover, then the archive. The target opens in MIGRATION MODE
    // (the review corrective's lifecycle): a fresh database receives
    // the persistent migration-pending marker, so an interrupted
    // migration can never be mistaken for a standalone campaign —
    // the normal opens refuse a pending target until the activation.
    let mut legacy = LocalFileMedium::open(legacy_root).map_err(MigrationError::Medium)?;
    let mut target =
        SqliteMedium::open_migration_target(target_db).map_err(MigrationError::Medium)?;
    let copied = copy_legacy_chain(&mut legacy, &mut target)?;
    cutover_seal_activate(legacy_root, &copied.target_summary.head, &mut target)?;
    // The handles must close before the archive rename: Windows
    // cannot rename a directory containing an open file (the legacy
    // medium holds `HEAD.lock`; the database connection is released
    // too, though it lives outside the archive).
    drop(legacy);
    drop(target);
    let archived_to = archive_legacy(legacy_root)?;
    Ok(MigrationReport {
        source_summary: Some(copied.source_summary),
        target_summary: copied.target_summary,
        copied: copied.appended,
        archived_to: Some(archived_to),
        resumed: false,
    })
}

fn io_err(op: &'static str, path: &Path, source: std::io::Error) -> MigrationError {
    MigrationError::Io {
        op,
        path: path.display().to_string(),
        source,
    }
}
