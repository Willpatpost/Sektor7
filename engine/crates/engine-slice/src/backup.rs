//! §6.2's quiescent, recovery-verified backup (PP-B1): capturing a
//! closed campaign's complete durable state — the SQLite canonical
//! database (`VACUUM INTO` over the ratified TRUNCATE checkpoint)
//! and the complete T1 journal file — bound by a manifest written
//! LAST, then proven restorable by an ACTUAL recovery in an isolated
//! scratch environment before the set is ever called usable.
//!
//! * **The quiescence is the caller's contract**: the tool runs
//!   between sessions with no canonical write, journal write, or
//!   journal rotation in flight across the entire capture (a
//!   graceful close, or a crashed session's quiet disk). The tool
//!   cannot verify the absence of a live process; it documents the
//!   contract and fails closed on everything it can verify.
//! * **The manifest binds the set**: the canonical head, the
//!   journal's SHA-256, the record count, and the campaign id —
//!   written last, atomically (tmp + fsync + rename). A set without
//!   it is refused as incomplete; one that will not decode is
//!   refused as corrupt; a set whose files do not match it is
//!   refused as incoherent (a tampered file, or a mixed-time pairing
//!   of one backup's database with another's journal).
//! * **Usable only after verification**: [`verify_backup`] re-reads
//!   the manifest, re-validates the captured chain (the actual
//!   canonical-chain validation, read-only), re-hashes the journal,
//!   and then RESTORES the pair into an isolated scratch directory
//!   and runs the real B4 recovery ([`SessionCrossing::recover_at`])
//!   over it. A set is never reported usable before that succeeds.
//! * **Never a quiet overwrite**: the backup refuses an existing
//!   output directory; the verification writes only into its
//!   ephemeral scratch; the source campaign receives no write except
//!   the ratified checkpoint that makes the capture clean. A failed
//!   capture leaves its incomplete directory behind as evidence —
//!   never a manifest, so every later verification refuses it.
//! * **Online (concurrent-writer) backup is deferred** (the
//!   ratified §6.2 scope): this tool is offline-only.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use engine_core::{Decoder, Encoder};
use engine_persist::hash::{Hash, hash_object};
use engine_persist::{CanonicalMedium, ChainSummary, MediumError, SqliteMedium};

use crate::crossing::SessionCrossing;
use crate::store::StoreLocation;

/// The manifest's wire domain (versioned, self-describing).
pub const BACKUP_MANIFEST_DOMAIN: &str = "sektor7/engine-slice/backup-manifest/1";
const BACKUP_MANIFEST_VERSION: u32 = 1;
const MANIFEST_FILE: &str = "MANIFEST";
const MANIFEST_TMP: &str = "MANIFEST.tmp";
const BACKUP_DB: &str = "store.db";
const BACKUP_JOURNAL: &str = "journal.t1";
/// The campaign-id wire bound (generous; the founding record's own
/// campaign ids are far shorter).
const MAX_CAMPAIGN_ID: u64 = 1024;

/// The backup tool's typed failures — fail-closed everywhere: a set
/// is never called usable on a failure path, and no partial state is
/// ever silently promoted.
#[derive(Debug)]
pub enum BackupError {
    /// The output directory already exists — never a quiet
    /// overwrite.
    Layout { detail: String },
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

/// The manifest's decoded binding.
struct Manifest {
    head: Hash,
    journal_sha256: Hash,
    records: u64,
    campaign_id: String,
}

fn manifest_bytes(manifest: &Manifest) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(BACKUP_MANIFEST_DOMAIN);
    enc.write_u32(BACKUP_MANIFEST_VERSION);
    enc.write_bytes(manifest.head.as_bytes());
    enc.write_bytes(manifest.journal_sha256.as_bytes());
    enc.write_u64(manifest.records);
    enc.write_str(&manifest.campaign_id);
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
    dec.finish().map_err(|e| corrupt(e.to_string()))?;
    Ok(Manifest {
        head,
        journal_sha256,
        records,
        campaign_id,
    })
}

/// §6.2's backup: capture the closed campaign's complete durable
/// state into a FRESH output directory, bind it with the manifest
/// written last, and prove it restorable — the isolated scratch
/// recovery runs before this returns, so `Ok` means usable. The
/// caller's contract: quiesced (no live session on either file — no
/// canonical write, journal write, or rotation in flight).
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
    fs::create_dir(output_dir).map_err(|e| io_err("create the backup directory", output_dir, e))?;
    // (1) The source must validate — the actual chain validation; a
    // corrupt source is refused before anything is captured.
    let mut source = SqliteMedium::open(db_path).map_err(BackupError::Medium)?;
    let summary = source.validate().map_err(BackupError::Medium)?;
    // (2) The capture: VACUUM INTO over the TRUNCATE checkpoint (the
    // quiescent fold of the WAL into the main file).
    let backup_db = output_dir.join(BACKUP_DB);
    source
        .backup_into(&backup_db)
        .map_err(BackupError::Medium)?;
    drop(source);
    // (3) The complete journal file (the lock sidecar is ephemeral
    // coordination, recreated on open — never campaign data).
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
    // the set complete.
    let manifest = Manifest {
        head: summary.head,
        journal_sha256,
        records: summary.records,
        campaign_id: summary.campaign_id.clone(),
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
/// hashing to the manifest's binding, and — the decisive proof — the
/// pair RESTORED into an isolated scratch directory and recovered
/// through the real B4 machinery. Returns the captured chain's
/// summary on success; every failure is a typed refusal, and a set
/// is never usable until this succeeds.
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
    // The restoration: an isolated scratch environment (never the
    // source, never a live campaign directory) and the real B4
    // recovery — the same machinery a restart uses.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| io_err("clock", backup_dir, std::io::Error::other(e)))?
        .as_nanos();
    let scratch = std::env::temp_dir().join(format!("s7-pp-b1-backup-verify-{stamp}"));
    fs::create_dir(&scratch).map_err(|e| io_err("create the scratch", &scratch, e))?;
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
