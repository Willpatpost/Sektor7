//! §6.2's quiescent, recovery-verified backup (PP-B1): capturing a
//! closed campaign's complete durable state — the SQLite canonical
//! database (`VACUUM INTO` over the ratified TRUNCATE checkpoint)
//! and the complete T1 journal file — bound by a manifest written
//! LAST, then proven restorable by an ACTUAL recovery in an isolated
//! scratch environment before the set is ever called usable.
//!
//! * **The offline capture boundary is established, not assumed**
//!   (the review corrective): the journal's sidecar lock is
//!   TRY-acquired and held across the ENTIRE capture — a live
//!   session's `T1Journal` holds that lock for its lifetime, so the
//!   acquisition either establishes the boundary mechanically or
//!   refuses (never blocks, never captures under a live session) —
//!   and the canonical head is verified STABLE across the capture
//!   (read before and after; any movement — a writer outside the
//!   journal discipline — refuses the set). A concurrent active
//!   session can never silently produce an allegedly coherent
//!   backup. The boundary covers both artifacts; the caller's
//!   remaining contract is the absence of a raw out-of-discipline
//!   writer between the reads, which the stability verification
//!   catches after the fact.
//! * **The manifest binds the set** — the complete ratified §6.2
//!   bindings (the review corrective): the canonical head, the
//!   journal's SHA-256, the record count, the campaign id, the last
//!   committed journal transaction, the journal's byte length, the
//!   checkpoint state, and the boundary evidence — written last,
//!   atomically (tmp + fsync + rename). A set without it is refused
//!   as incomplete; one that will not decode is refused as corrupt;
//!   a set whose files do not match it is refused as incoherent (a
//!   tampered file, or a mixed-time pairing of one backup's database
//!   with another's journal).
//! * **Usable only after verification**: [`verify_backup`] re-reads
//!   the manifest, re-validates the captured chain (the actual
//!   canonical-chain validation, read-only), re-hashes the journal,
//!   RE-SCANS it (the last committed transaction and the byte length
//!   must match the binding), and then RESTORES the pair into an
//!   isolated scratch directory and runs the real B4 recovery
//!   ([`SessionCrossing::recover_at`]) over it. A set is never
//!   reported usable before that succeeds.
//! * **Never a quiet overwrite**: the backup refuses an existing
//!   output directory; the verification writes only into its
//!   ephemeral scratch; the source campaign receives no write except
//!   the ratified checkpoint that makes the capture clean. A failed
//!   capture leaves its incomplete directory behind as evidence —
//!   never a manifest, so every later verification refuses it.
//! * **Online (concurrent-writer) backup is deferred** (the
//!   ratified §6.2 scope): this tool is offline-only — it refuses
//!   concurrent activity, it never captures through it.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use engine_core::{Decoder, Encoder};
use engine_persist::hash::{Hash, hash_object};
use engine_persist::{
    CanonicalMedium, ChainSummary, MediumError, SqliteMedium, T1Journal, active_journal_path,
};

use crate::crossing::SessionCrossing;
use crate::store::StoreLocation;

/// The manifest's wire domain (versioned, self-describing).
pub const BACKUP_MANIFEST_DOMAIN: &str = "sektor7/engine-slice/backup-manifest/1";
/// The manifest's wire version. **v3** (the final review's contract
/// completion) adds the source-journal identity and the capture
/// timestamp to the v2 binding set; a version-2 manifest is REFUSED
/// by the version check — never silently reinterpreted.
const BACKUP_MANIFEST_VERSION: u32 = 3;
const MANIFEST_FILE: &str = "MANIFEST";
const MANIFEST_TMP: &str = "MANIFEST.tmp";
const BACKUP_DB: &str = "store.db";
const BACKUP_JOURNAL: &str = "journal.t1";
/// The campaign-id wire bound (generous; the founding record's own
/// campaign ids are far shorter).
const MAX_CAMPAIGN_ID: u64 = 1024;
/// The source-journal identity's wire bound (a path; generous).
const MAX_SOURCE_JOURNAL: u64 = 4096;
/// The none-sentinel for the last-committed-transaction binding (a
/// journal with no committed transactions): no real content hashes
/// to the zero digest.
const NONE_TX: [u8; 32] = [0u8; 32];

/// The backup tool's typed failures — fail-closed everywhere: a set
/// is never called usable on a failure path, and no partial state is
/// ever silently promoted.
#[derive(Debug)]
pub enum BackupError {
    /// The output directory already exists — never a quiet
    /// overwrite.
    Layout { detail: String },
    /// The offline capture boundary is not established: a live
    /// session holds the journal's lock, or the canonical head moved
    /// during the capture — a concurrent active session must never
    /// silently produce an allegedly coherent backup.
    Quiescence { detail: String },
    /// The set is incomplete (no manifest — the capture never
    /// completed — or a captured file is absent).
    Incomplete { detail: String },
    /// The manifest will not decode — the binding evidence is
    /// corrupt; nothing is guessed.
    ManifestCorrupt { detail: String },
    /// The set is incoherent: the captured files do not match the
    /// manifest's binding (a tampered file, or a mixed-time pairing
    /// of one backup's database with another's journal).
    Incoherent { detail: String },
    /// The scratch restoration did not recover through the real B4
    /// machinery — the set is not usable, whatever its bindings say.
    Verification { detail: String },
    /// A medium-level failure, carried through.
    Medium(MediumError),
    /// Filesystem I/O during the tool's own file operations.
    Io {
        op: &'static str,
        path: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::Layout { detail } => {
                write!(f, "the backup layout is unsafe: {detail}")
            }
            BackupError::Quiescence { detail } => {
                write!(
                    f,
                    "the offline capture boundary is not established: {detail}"
                )
            }
            BackupError::Incomplete { detail } => {
                write!(f, "the backup set is incomplete: {detail}")
            }
            BackupError::ManifestCorrupt { detail } => {
                write!(f, "the backup manifest is corrupt: {detail}")
            }
            BackupError::Incoherent { detail } => {
                write!(f, "the backup set is incoherent: {detail}")
            }
            BackupError::Verification { detail } => {
                write!(f, "the backup did not verify: {detail}")
            }
            BackupError::Medium(error) => write!(f, "medium failure: {error}"),
            BackupError::Io { op, path, source } => {
                write!(f, "backup I/O failure during {op} at {path}: {source}")
            }
        }
    }
}

impl std::error::Error for BackupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BackupError::Medium(error) => Some(error),
            BackupError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// What a verified backup established.
#[derive(Debug, Clone)]
pub struct BackupReport {
    /// The completed, verified backup-set directory (the database,
    /// the journal, and the binding manifest).
    pub backup_dir: PathBuf,
    /// The captured chain's validated summary (the source's, proven
    /// identical in the captured database).
    pub summary: ChainSummary,
    /// The captured journal's SHA-256 (the manifest's binding).
    pub journal_sha256: Hash,
}

/// The offline capture boundary's journal half (the review
/// corrective): the T1 sidecar lock (`<journal>.lock` — the same
/// construction the journal itself uses), try-acquired — a live
/// session's `T1Journal` holds this lock for its lifetime, so a held
/// lock IS a live session and the backup refuses rather than
/// blocking. Held across the ENTIRE capture (no journal write or
/// rotation can interleave); released before the audit and manifest
/// phases (which touch only the captured files).
struct JournalGuard(File);

impl JournalGuard {
    fn try_acquire(journal_path: &Path) -> Result<Self, BackupError> {
        let mut name = match journal_path.file_name() {
            Some(name) => name.to_os_string(),
            None => {
                return Err(BackupError::Layout {
                    detail: format!(
                        "the journal path {} has no file name",
                        journal_path.display()
                    ),
                });
            }
        };
        name.push(".lock");
        let lock_path = journal_path.with_file_name(name);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| io_err("open the journal lock", &lock_path, e))?;
        file.try_lock().map_err(|e| BackupError::Quiescence {
            detail: format!(
                "the journal's sidecar lock could not be acquired — a live \
                     session holds it (or the OS refused: {e}); never capture \
                     under a live session",
            ),
        })?;
        Ok(JournalGuard(file))
    }
}

impl Drop for JournalGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The manifest's decoded binding.
struct Manifest {
    head: Hash,
    journal_sha256: Hash,
    records: u64,
    campaign_id: String,
    last_committed_tx: Hash,
    journal_bytes: u64,
    /// The journal actually selected and locked for the capture —
    /// PROVENANCE ONLY (the active journal's identity at capture;
    /// under a future rotation protocol, the post-rotation journal
    /// actually locked): the verification and the scratch
    /// restoration never require the original path — the set is
    /// portable; the field exists so an operator can identify which
    /// journal file a set was captured from.
    source_journal: String,
    /// The capture timestamp (Unix-epoch nanoseconds) — OBSERVATIONAL
    /// METADATA ONLY: when the capturing machine's clock claimed the
    /// capture ran. NEVER authoritative ordering evidence (the chain
    /// head, the journal bindings, and the record counts order the
    /// set) and NEVER durability evidence (the commits and fsyncs
    /// establish durability); a restored or moved set keeps its
    /// original timestamp unchanged.
    captured_at_nanos: u64,
    checkpointed: bool,
    journal_locked: bool,
    head_stable: bool,
}

fn manifest_bytes(manifest: &Manifest) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(BACKUP_MANIFEST_DOMAIN);
    enc.write_u32(BACKUP_MANIFEST_VERSION);
    enc.write_bytes(manifest.head.as_bytes());
    enc.write_bytes(manifest.journal_sha256.as_bytes());
    enc.write_u64(manifest.records);
    enc.write_str(&manifest.campaign_id);
    enc.write_bytes(manifest.last_committed_tx.as_bytes());
    enc.write_u64(manifest.journal_bytes);
    enc.write_str(&manifest.source_journal);
    enc.write_u64(manifest.captured_at_nanos);
    enc.write_u32(manifest.checkpointed as u32);
    enc.write_u32(manifest.journal_locked as u32);
    enc.write_u32(manifest.head_stable as u32);
    enc.into_bytes()
}

fn decode_manifest(bytes: &[u8]) -> Result<Manifest, BackupError> {
    let corrupt = |detail: String| BackupError::ManifestCorrupt { detail };
    let mut dec = Decoder::new(bytes);
    dec.read_domain(BACKUP_MANIFEST_DOMAIN)
        .map_err(|e| corrupt(e.to_string()))?;
    let version = dec.read_u32().map_err(|e| corrupt(e.to_string()))?;
    if version != BACKUP_MANIFEST_VERSION {
        return Err(corrupt(format!("unsupported manifest version {version}")));
    }
    let head = Hash::from_bytes(
        dec.read_bytes(Hash::WIDTH, "backup-head")
            .map_err(|e| corrupt(e.to_string()))?,
    )
    .ok_or_else(|| corrupt("head width".to_owned()))?;
    let journal_sha256 = Hash::from_bytes(
        dec.read_bytes(Hash::WIDTH, "journal-hash")
            .map_err(|e| corrupt(e.to_string()))?,
    )
    .ok_or_else(|| corrupt("journal hash width".to_owned()))?;
    let records = dec.read_u64().map_err(|e| corrupt(e.to_string()))?;
    let campaign_id = dec
        .read_str(MAX_CAMPAIGN_ID, "backup-campaign")
        .map_err(|e| corrupt(e.to_string()))?
        .to_owned();
    let last_committed_tx = Hash::from_bytes(
        dec.read_bytes(Hash::WIDTH, "backup-last-committed")
            .map_err(|e| corrupt(e.to_string()))?,
    )
    .ok_or_else(|| corrupt("last-committed width".to_owned()))?;
    let journal_bytes = dec.read_u64().map_err(|e| corrupt(e.to_string()))?;
    // The source-journal identity (the final review's contract): the
    // provenance of the journal actually locked for the capture —
    // present and bounded, never a restoration requirement.
    let source_journal = dec
        .read_str(MAX_SOURCE_JOURNAL, "backup-source-journal")
        .map_err(|e| corrupt(e.to_string()))?
        .to_owned();
    if source_journal.is_empty() {
        return Err(corrupt(
            "the source-journal identity is absent — the manifest must \
             name the journal actually locked for the capture"
                .to_owned(),
        ));
    }
    // The capture timestamp (observational — never ordering or
    // durability evidence; any value decodes).
    let captured_at_nanos = dec.read_u64().map_err(|e| corrupt(e.to_string()))?;
    let checkpointed = dec.read_u32().map_err(|e| corrupt(e.to_string()))? != 0;
    let journal_locked = dec.read_u32().map_err(|e| corrupt(e.to_string()))? != 0;
    let head_stable = dec.read_u32().map_err(|e| corrupt(e.to_string()))? != 0;
    dec.finish().map_err(|e| corrupt(e.to_string()))?;
    Ok(Manifest {
        head,
        journal_sha256,
        records,
        campaign_id,
        last_committed_tx,
        journal_bytes,
        source_journal,
        captured_at_nanos,
        checkpointed,
        journal_locked,
        head_stable,
    })
}

/// The none-sentinel for the last-committed-transaction binding.
fn none_tx() -> Hash {
    Hash::from_bytes(&NONE_TX).expect("the zero digest decodes")
}

/// The journal's last committed transaction, scanned from the
/// CAPTURED copy (the none sentinel when the journal holds none).
fn scan_last_committed(journal: &Path) -> Result<Hash, BackupError> {
    let scan = T1Journal::scan_file(journal).map_err(|e| BackupError::Incoherent {
        detail: format!("the journal does not parse: {e}"),
    })?;
    Ok(scan
        .committed
        .last()
        .map(|transaction| transaction.id.0)
        .unwrap_or_else(none_tx))
}

/// §6.2's backup: capture the closed campaign's complete durable
/// state into a FRESH output directory, bound by the manifest
/// written last, and prove it restorable — the isolated scratch
/// recovery runs before this returns, so `Ok` means usable. The
/// offline boundary is established mechanically (the journal lock)
/// and verified (the head stability); a concurrent active session
/// refuses the backup, never a silent capture.
pub fn backup_campaign(
    db_path: &Path,
    journal_path: &Path,
    output_dir: &Path,
) -> Result<BackupReport, BackupError> {
    // Never a quiet overwrite: a fresh directory only.
    if output_dir.exists() {
        return Err(BackupError::Layout {
            detail: format!(
                "the output directory {} already exists — refusing to overwrite it",
                output_dir.display()
            ),
        });
    }
    // (0) THE OFFLINE CAPTURE BOUNDARY (the review corrective): the
    // journal's sidecar lock, try-acquired and held across the
    // ENTIRE capture — a live session's T1Journal holds it for its
    // lifetime, so the acquisition either establishes the boundary
    // mechanically or refuses (never blocks, never captures under a
    // live session). THE active-journal resolution (PP-B3): the
    // rotation's pointer beside the legacy path names the rotated
    // journal — the boundary, the captured bytes, and the manifest's
    // source-journal provenance all bind THE ACTIVE journal; a
    // missing pointer resolves the legacy itself (the pre-rotation
    // compatibility: every existing campaign backs up identically).
    let journal_path =
        &active_journal_path(journal_path).map_err(|error| BackupError::Incoherent {
            detail: format!("the active-journal pointer does not resolve: {error}"),
        })?;
    let journal_guard = JournalGuard::try_acquire(journal_path)?;
    fs::create_dir(output_dir).map_err(|e| io_err("create the backup directory", output_dir, e))?;
    // (1) The source must validate — the actual chain validation; a
    // corrupt source is refused before anything is captured.
    let mut source = SqliteMedium::open(db_path).map_err(BackupError::Medium)?;
    let summary = source.validate().map_err(BackupError::Medium)?;
    // (2) THE CAPTURE with the head-stability verification (the
    // canonical-write half of the boundary): the head is read before
    // and after the capture, and any movement — a writer outside the
    // journal discipline — refuses the set as not quiescent. The
    // capture itself is `VACUUM INTO` over the TRUNCATE checkpoint
    // (the quiescent fold of the WAL into the main file; a live
    // writer mid-transaction contends the checkpoint — the typed
    // refusal, never a silent capture).
    let head_before = source.head().map_err(BackupError::Medium)?;
    let backup_db = output_dir.join(BACKUP_DB);
    source
        .backup_into(&backup_db)
        .map_err(BackupError::Medium)?;
    let head_after = source.head().map_err(BackupError::Medium)?;
    if head_before != head_after {
        let before = head_before
            .map(|hash| hash.to_string())
            .unwrap_or_else(|| "unfounded".to_owned());
        let after = head_after
            .map(|hash| hash.to_string())
            .unwrap_or_else(|| "unfounded".to_owned());
        return Err(BackupError::Quiescence {
            detail: format!(
                "the canonical head moved during the capture (before {before}, \
                 after {after}) — a concurrent writer is active; never capture a \
                 moving campaign",
            ),
        });
    }
    drop(source);
    // (3) The complete journal file, copied under the held lock (the
    // lock sidecar is ephemeral coordination, recreated on open —
    // never campaign data).
    if !journal_path.is_file() {
        return Err(BackupError::Incomplete {
            detail: format!(
                "the journal {} is absent — a campaign backup requires the pair",
                journal_path.display()
            ),
        });
    }
    let backup_journal = output_dir.join(BACKUP_JOURNAL);
    fs::copy(journal_path, &backup_journal)
        .map_err(|e| io_err("copy the journal", journal_path, e))?;
    // The boundary's work is done — release the journal lock before
    // the audit, manifest, and verification phases (they touch only
    // the captured files).
    drop(journal_guard);
    // (4) The captured set's own coherence: the database validates to
    // the source's chain; the journal hash binds.
    let journal_bytes = fs::read(&backup_journal)
        .map_err(|e| io_err("read the captured journal", &backup_journal, e))?;
    let journal_sha256 = hash_object(&journal_bytes);
    let mut audit = SqliteMedium::open_read_only(&backup_db).map_err(BackupError::Medium)?;
    let captured = audit.validate().map_err(BackupError::Medium)?;
    drop(audit);
    if captured != summary {
        return Err(BackupError::Incoherent {
            detail: "the captured database does not validate to the source chain".to_owned(),
        });
    }
    // (5) The manifest LAST, atomically — its presence is what marks
    // the set complete — carrying the complete ratified §6.2
    // bindings: the head, the journal hash, the records, the campaign
    // id, the last committed transaction, the journal length, the
    // SOURCE-JOURNAL IDENTITY (the journal actually selected and
    // locked — provenance, never a restoration requirement), the
    // CAPTURE TIMESTAMP (observational, never ordering or durability
    // evidence), the checkpoint state, and the boundary evidence.
    let captured_at_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| io_err("clock", journal_path, std::io::Error::other(e)))?
        .as_nanos() as u64;
    let manifest = Manifest {
        head: summary.head,
        journal_sha256,
        records: summary.records,
        campaign_id: summary.campaign_id.clone(),
        last_committed_tx: scan_last_committed(&backup_journal)?,
        journal_bytes: journal_bytes.len() as u64,
        source_journal: journal_path.to_string_lossy().to_string(),
        captured_at_nanos,
        checkpointed: true,
        journal_locked: true,
        head_stable: true,
    };
    write_manifest(output_dir, &manifest)?;
    // (6) Usable only after the verification: the isolated scratch
    // restoration and the real B4 recovery.
    verify_backup(output_dir)?;
    Ok(BackupReport {
        backup_dir: output_dir.to_path_buf(),
        summary,
        journal_sha256,
    })
}

/// Verifies a backup set: the manifest present and decodable, the
/// captured chain validating to the manifest's binding, the journal
/// hashing AND re-scanning to the manifest's binding (the last
/// committed transaction and the byte length), the boundary evidence
/// present, and — the decisive proof — the pair RESTORED into an
/// isolated scratch directory and recovered through the real B4
/// machinery. Returns the captured chain's summary on success; every
/// failure is a typed refusal, and a set is never usable until this
/// succeeds.
pub fn verify_backup(backup_dir: &Path) -> Result<ChainSummary, BackupError> {
    let manifest_path = backup_dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(BackupError::Incomplete {
            detail: format!(
                "no manifest at {} — the capture never completed (the manifest is written last)",
                manifest_path.display()
            ),
        });
    }
    let bytes =
        fs::read(&manifest_path).map_err(|e| io_err("read the manifest", &manifest_path, e))?;
    let manifest = decode_manifest(&bytes)?;
    let backup_db = backup_dir.join(BACKUP_DB);
    if !backup_db.is_file() {
        return Err(BackupError::Incomplete {
            detail: "the captured database is absent".to_owned(),
        });
    }
    let backup_journal = backup_dir.join(BACKUP_JOURNAL);
    if !backup_journal.is_file() {
        return Err(BackupError::Incomplete {
            detail: "the captured journal is absent".to_owned(),
        });
    }
    // The bindings: the actual chain validation and the journal hash.
    let mut audit = SqliteMedium::open_read_only(&backup_db).map_err(BackupError::Medium)?;
    let summary = audit.validate().map_err(BackupError::Medium)?;
    drop(audit);
    if summary.head != manifest.head || summary.records != manifest.records {
        return Err(BackupError::Incoherent {
            detail: format!(
                "the captured chain (head {}, {} records) does not match the manifest \
                 (head {}, {} records) — a tampered database or a mixed-time pairing",
                summary.head, summary.records, manifest.head, manifest.records
            ),
        });
    }
    let journal_bytes = fs::read(&backup_journal)
        .map_err(|e| io_err("read the captured journal", &backup_journal, e))?;
    if hash_object(&journal_bytes) != manifest.journal_sha256 {
        return Err(BackupError::Incoherent {
            detail: "the captured journal does not hash to the manifest's binding \
                     — a tampered journal or a mixed-time pairing"
                .to_owned(),
        });
    }
    // The complete §6.2 bindings (the review corrective): the journal
    // RE-SCAN — the last committed transaction and the byte length
    // must match the manifest's binding (a parse of the actual
    // journal, not just its hash).
    if scan_last_committed(&backup_journal)? != manifest.last_committed_tx {
        return Err(BackupError::Incoherent {
            detail: "the captured journal's last committed transaction does not \
                     match the manifest's binding — a tampered journal"
                .to_owned(),
        });
    }
    if journal_bytes.len() as u64 != manifest.journal_bytes {
        return Err(BackupError::Incoherent {
            detail: "the captured journal's length does not match the manifest's \
                     binding — a tampered journal"
                .to_owned(),
        });
    }
    // The boundary evidence: the capture path always records the
    // enforced offline boundary — a hand-crafted manifest without it
    // is refused (the set was not captured through the discipline).
    if !manifest.checkpointed || !manifest.journal_locked || !manifest.head_stable {
        return Err(BackupError::ManifestCorrupt {
            detail: "the manifest's boundary evidence is absent — the set was not \
                     captured through the offline boundary discipline"
                .to_owned(),
        });
    }
    // The restoration: an isolated scratch environment (never the
    // source, never a live campaign directory) and the real B4
    // recovery — the same machinery a restart uses. The scratch
    // name carries the process id AND a nanosecond stamp, and the
    // directory is created EXCLUSIVELY (never `create_dir_all` — a
    // collision must surface, never silently share a scratch): on
    // an AlreadyExists collision (a coarse clock handing two
    // parallel verifications the same stamp) a bounded retry
    // regenerates the name, and the cleanup below removes only the
    // directory THIS verification successfully claimed.
    let scratch = {
        const CLAIM_ATTEMPTS: u32 = 8;
        let mut claimed: Option<PathBuf> = None;
        let mut last_candidate = std::env::temp_dir();
        for attempt in 0..CLAIM_ATTEMPTS {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| io_err("clock", backup_dir, std::io::Error::other(e)))?
                .as_nanos();
            let candidate = std::env::temp_dir().join(format!(
                "s7-pp-b1-backup-verify-{}-{stamp}",
                std::process::id()
            ));
            last_candidate = candidate.clone();
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    claimed = Some(candidate);
                    break;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && attempt + 1 < CLAIM_ATTEMPTS =>
                {
                    // Another verification claimed this name —
                    // regenerate and retry (bounded).
                    continue;
                }
                Err(error) => {
                    return Err(io_err("create the scratch", &candidate, error));
                }
            }
        }
        match claimed {
            Some(scratch) => scratch,
            None => {
                // The bounded retry's exhaustion is a typed refusal
                // — never a partial or shared scratch environment.
                return Err(io_err(
                    "create the scratch",
                    &last_candidate,
                    std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "the scratch-name collision retry is exhausted",
                    ),
                ));
            }
        }
    };
    let restoration = (|| -> Result<(), BackupError> {
        let scratch_db = scratch.join(BACKUP_DB);
        let scratch_journal = scratch.join(BACKUP_JOURNAL);
        fs::copy(&backup_db, &scratch_db)
            .map_err(|e| io_err("restore the database into the scratch", &scratch_db, e))?;
        fs::copy(&backup_journal, &scratch_journal)
            .map_err(|e| io_err("restore the journal into the scratch", &scratch_journal, e))?;
        let (_spine, _director, crossing) = SessionCrossing::recover_at(
            &StoreLocation::Database(scratch_db),
            &scratch_journal,
            &manifest.campaign_id,
        )
        .map_err(|e| BackupError::Verification {
            detail: format!("the restored pair does not recover: {e:?}"),
        })?;
        drop(crossing);
        Ok(())
    })();
    let _ = fs::remove_dir_all(&scratch);
    restoration?;
    Ok(summary)
}

fn write_manifest(output_dir: &Path, manifest: &Manifest) -> Result<(), BackupError> {
    let tmp = output_dir.join(MANIFEST_TMP);
    let path = output_dir.join(MANIFEST_FILE);
    let mut file = File::create(&tmp).map_err(|e| io_err("create MANIFEST.tmp", &tmp, e))?;
    file.write_all(&manifest_bytes(manifest))
        .map_err(|e| io_err("write MANIFEST.tmp", &tmp, e))?;
    file.sync_all()
        .map_err(|e| io_err("sync MANIFEST.tmp", &tmp, e))?;
    drop(file);
    fs::rename(&tmp, &path).map_err(|e| io_err("publish the manifest", &path, e))?;
    let _ = File::open(output_dir).and_then(|dir| dir.sync_all());
    Ok(())
}

fn io_err(op: &'static str, path: &Path, source: std::io::Error) -> BackupError {
    BackupError::Io {
        op,
        path: path.display().to_string(),
        source,
    }
}
