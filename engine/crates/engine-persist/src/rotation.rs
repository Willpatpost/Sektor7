//! The durable T1 journal rotation (PP-B3, D-PP-4's conservative
//! journal rotation under the §4.4 survival rules): the surviving
//! transactions and cancellation records are written to a FRESH
//! journal file, RE-ANCHORED under the new file's own hash chain
//! (the documented choice — verbatim frames would break the record
//! chain over the dropped span, while the transaction identities are
//! content-derived, so duplicate recognition, cancellation
//! suppression, and recovery replay all survive the re-encoding);
//! the new file is fully scan-verified BEFORE the flip; and the
//! active-path flip is the atomic tmp+fsync+rename of a POINTER file
//! beside the legacy journal. The legacy journal is retained
//! untouched until the reclamation boundary decides its removal —
//! never deleted here.
//!
//! **The crash windows:** a death before the flip leaves the pointer
//! naming the old journal (or absent — the pre-rotation state): the
//! old journal is a superset of the survival set, nothing is lost,
//! and the orphaned new file is inert evidence. The flip itself is
//! atomic. A death after the flip leaves the verified survival set
//! active. The rotation is an explicit operator action under the
//! single-writer discipline — never automatic, never on a silent
//! threshold.
//!
//! **The pointer:** `<legacy-journal>.active` — a small file naming
//! the active journal's FILE NAME (relative to the legacy journal's
//! directory; bounded, UTF-8, no separators — fail closed on
//! anything else, never guessed). A missing pointer resolves the
//! legacy journal itself (the pre-rotation compatibility: every
//! existing campaign and backup behaves identically).

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use engine_core::SaveError;

use crate::journal::{
    CommittedTransaction, JournalError, RecordedCancellation, T1Journal, TailState, TransactionId,
    TransactionInput, io_err, sync_parent_dir,
};

/// The active-path pointer's suffix (beside the legacy journal).
pub const JOURNAL_ACTIVE_SUFFIX: &str = ".active";

/// The pointer's bounded content (a file name).
const MAX_ACTIVE_NAME_BYTES: u64 = 255;

/// The rotation's report — the exact accounting of what moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRotation {
    /// The new active journal (the pointer now names it).
    pub new_path: PathBuf,
    /// The journal this rotation read — retained on disk, untouched;
    /// the reclamation boundary decides its eventual removal.
    pub legacy_path: PathBuf,
    /// The surviving transaction identities, re-anchored in order.
    pub retained: Vec<TransactionId>,
    /// The retired transaction identities (subsumed or canceled —
    /// dropped from the active set; their frames remain in the
    /// retained legacy file as evidence).
    pub retired: Vec<TransactionId>,
    /// The cancellation records carried over (each surviving only
    /// while its target survives).
    pub retained_cancellations: u64,
}

/// Resolves the campaign's active journal: the pointer file beside
/// `legacy_path` names the rotated journal; a missing pointer
/// resolves the legacy path itself (the pre-rotation compatibility).
/// A present-but-corrupt pointer fails closed — an ambiguous active
/// journal is never guessed.
pub fn active_journal_path(legacy_path: &Path) -> Result<PathBuf, JournalError> {
    let pointer = pointer_path(legacy_path);
    match fs::metadata(&pointer) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(legacy_path.to_path_buf());
        }
        Err(e) => return Err(io_err("stat journal pointer", &pointer, e)),
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(JournalError::Format(SaveError::Integrity(
                    "journal-pointer/not-a-file",
                )));
            }
            if metadata.len() > MAX_ACTIVE_NAME_BYTES {
                return Err(JournalError::Format(SaveError::Limit {
                    section: "journal-pointer",
                    kind: "name-length",
                    value: metadata.len(),
                    max: MAX_ACTIVE_NAME_BYTES,
                }));
            }
        }
    }
    let bytes = fs::read(&pointer).map_err(|e| io_err("read journal pointer", &pointer, e))?;
    let name = std::str::from_utf8(&bytes)
        .map_err(|_| JournalError::Format(SaveError::Integrity("journal-pointer/utf8")))?;
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(JournalError::Format(SaveError::Integrity(
            "journal-pointer/name",
        )));
    }
    Ok(legacy_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(name))
}

/// The pointer file's path (beside the legacy journal).
fn pointer_path(legacy_path: &Path) -> PathBuf {
    let mut name = legacy_path.file_name().unwrap_or_default().to_os_string();
    name.push(JOURNAL_ACTIVE_SUFFIX);
    legacy_path.with_file_name(name)
}

/// Writes the surviving set to a fresh journal, scan-verifies it,
/// and atomically flips the active-path pointer. The legacy journal
/// is never modified (its bytes and its sidecar lock stand). The
/// `retired` list is report evidence only — the caller's
/// classification, recorded honestly.
pub fn rotate_journal(
    legacy_path: &Path,
    retained: &[CommittedTransaction],
    retained_cancellations: &[RecordedCancellation],
    retired: &[TransactionId],
) -> Result<JournalRotation, JournalError> {
    // (1) The fresh path (unique by construction; the legacy
    // journal's directory).
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| io_err("clock", legacy_path, std::io::Error::other(e)))?
        .as_nanos();
    let new_path = legacy_path.with_file_name(format!("journal-{nanos}.t1"));
    // (2) The write: a fresh journal (its own header — the re-anchor
    // root — and its own sidecar lock), each survivor re-appended as
    // a complete transaction under the new chain. The transaction
    // identity is content-derived, so each re-append reproduces the
    // same identity and payload — the dedup semantics of the active
    // set survive the re-encoding.
    {
        let mut journal = T1Journal::open(&new_path)?;
        for transaction in retained {
            let input = TransactionInput {
                envelopes: transaction.envelopes.clone(),
                position: transaction.position,
                classification: transaction.classification,
                dependencies: transaction.dependencies.clone(),
                frontier: transaction.frontier.clone(),
                plan: transaction.plan.clone(),
            };
            journal.append_transaction(&input)?;
        }
        for cancellation in retained_cancellations {
            journal.append_cancellation(
                cancellation.target,
                cancellation.reason,
                &cancellation.detail,
            )?;
        }
        // The instance drops here — the new journal's sidecar lock
        // releases before the caller may reopen it.
    }
    // (3) The verification — a full read-only scan of the new file
    // BEFORE any flip: exactly the retained identities in order with
    // their payloads, exactly the retained cancellations, a clean
    // tail. Anything else fails closed — the pointer never names an
    // unverified journal.
    let scan = T1Journal::scan_file(&new_path)?;
    let rejected = || JournalError::Format(SaveError::Integrity("journal-rotation/verification"));
    if scan.committed.len() != retained.len() {
        return Err(rejected());
    }
    for (expected, observed) in retained.iter().zip(&scan.committed) {
        if expected.id != observed.id || expected.envelopes != observed.envelopes {
            return Err(rejected());
        }
    }
    if scan.cancellations.len() != retained_cancellations.len() {
        return Err(rejected());
    }
    for (expected, observed) in retained_cancellations.iter().zip(&scan.cancellations) {
        if expected.target != observed.target || expected.reason != observed.reason {
            return Err(rejected());
        }
    }
    if scan.tail != TailState::Clean {
        return Err(rejected());
    }
    // (4) THE DURABLE ACTIVE-PATH FLIP: tmp + fsync + rename onto
    // the pointer, then the directory sync — atomic, so a crash
    // leaves either the pre-rotation pointer state or the completed
    // flip, never a torn pointer.
    let name = new_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(JournalError::Format(SaveError::Integrity(
            "journal-rotation/name",
        )))?;
    let tmp_path = legacy_path.with_file_name(format!(
        "{}{JOURNAL_ACTIVE_SUFFIX}.tmp",
        legacy_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    let mut file =
        File::create(&tmp_path).map_err(|e| io_err("create journal pointer tmp", &tmp_path, e))?;
    file.write_all(name.as_bytes())
        .map_err(|e| io_err("write journal pointer tmp", &tmp_path, e))?;
    file.sync_all()
        .map_err(|e| io_err("sync journal pointer tmp", &tmp_path, e))?;
    drop(file);
    fs::rename(&tmp_path, pointer_path(legacy_path))
        .map_err(|e| io_err("flip journal pointer", legacy_path, e))?;
    sync_parent_dir(&pointer_path(legacy_path))
        .map_err(|e| io_err("sync journal dir", legacy_path, e))?;
    Ok(JournalRotation {
        new_path,
        legacy_path: legacy_path.to_path_buf(),
        retained: retained.iter().map(|transaction| transaction.id).collect(),
        retired: retired.to_vec(),
        retained_cancellations: retained_cancellations.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consequence::{AwardCheckpoint, ConsequenceId, Coverage, OccurrenceSpan};
    use crate::envelope::{
        ClassifiedSpan, ConsequenceEnvelope, CounterValues, DurabilityClass, SignalLane,
        SourceBaseline, SourcePosition,
    };
    use crate::journal::{CancellationReason, FrontierEntry};

    /// A temporary journal directory (the journal.rs test pattern).
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "s7-pp-b3-rotation-{name}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            TestDir(path)
        }

        fn journal(&self) -> PathBuf {
            self.0.join("journal.t1")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The journal.rs test fixture's transaction shape (one envelope,
    /// one frontier entry, a distinguishable plan).
    fn tx_input(index: u64) -> TransactionInput {
        let identity = ConsequenceId::new(1, index, 1);
        let envelope = ConsequenceEnvelope::capture(
            identity,
            vec![ClassifiedSpan {
                span: OccurrenceSpan::new(2 * (index - 1), 2 * index).unwrap(),
                class: DurabilityClass::DurableEligible,
            }],
            SignalLane::Captured,
            CounterValues::default(),
            SourceBaseline {
                counters: CounterValues::default(),
                award: AwardCheckpoint {
                    durable: Coverage::new(),
                    provisional: Coverage::new(),
                },
            },
            SourcePosition {
                head: None,
                tick: 100 + index,
            },
        )
        .unwrap();
        TransactionInput {
            envelopes: vec![envelope],
            position: SourcePosition {
                head: None,
                tick: 100 + index,
            },
            classification: DurabilityClass::DurableEligible,
            dependencies: Vec::new(),
            frontier: vec![FrontierEntry {
                region: 1,
                episode: index,
            }],
            plan: format!("plan-{index}").into_bytes(),
        }
    }

    #[test]
    fn the_rotation_re_anchors_survivors_and_flips_the_pointer() {
        // THE mechanism over a real journal: two transactions, the
        // second canceled; the rotation retains the first, retires
        // the second. The new file carries exactly the survivor
        // (identity and payload — the digest and sequence honestly
        // differ, the re-anchoring), the legacy journal is
        // byte-identical, and the pointer resolves the new file.
        let dir = TestDir::new("mechanism");
        let legacy = dir.journal();
        let mut journal = T1Journal::open(&legacy).unwrap();
        let first = journal.append_transaction(&tx_input(1)).unwrap();
        let second = journal.append_transaction(&tx_input(2)).unwrap();
        journal
            .append_cancellation(second.id, CancellationReason::ApplicationPolicy, b"retired")
            .unwrap();
        let survivors: Vec<CommittedTransaction> = journal
            .transactions()
            .iter()
            .filter(|transaction| transaction.id == first.id)
            .cloned()
            .collect();
        assert_eq!(survivors.len(), 1);
        let legacy_bytes = fs::read(&legacy).unwrap();
        drop(journal);

        let rotation = rotate_journal(&legacy, &survivors, &[], &[second.id]).unwrap();

        // The pointer resolves the new file; the legacy stands.
        assert_eq!(active_journal_path(&legacy).unwrap(), rotation.new_path);
        assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
        // The new file: exactly the survivor — same identity, same
        // payload — no cancellations, a clean tail.
        let scan = T1Journal::scan_file(&rotation.new_path).unwrap();
        assert_eq!(scan.committed.len(), 1);
        assert_eq!(scan.committed[0].id, first.id);
        assert_eq!(scan.committed[0].envelopes, survivors[0].envelopes);
        assert!(scan.cancellations.is_empty());
        assert_eq!(scan.tail, TailState::Clean);
        // The report accounts exactly.
        assert_eq!(rotation.retained, vec![first.id]);
        assert_eq!(rotation.retired, vec![second.id]);
        assert_eq!(rotation.retained_cancellations, 0);
        assert_eq!(rotation.legacy_path, legacy);
    }

    #[test]
    fn the_re_anchored_transaction_recognizes_idempotent_reappends() {
        // THE dedup-preservation proof: re-appending a survivor's
        // input to the re-anchored journal is recognized idempotently
        // (the identity is content-derived) — no duplicate frames,
        // the existing committed state returns.
        let dir = TestDir::new("idempotent");
        let legacy = dir.journal();
        let mut journal = T1Journal::open(&legacy).unwrap();
        let first = journal.append_transaction(&tx_input(1)).unwrap();
        let survivors: Vec<CommittedTransaction> = journal
            .transactions()
            .iter()
            .filter(|transaction| transaction.id == first.id)
            .cloned()
            .collect();
        drop(journal);
        let rotation = rotate_journal(&legacy, &survivors, &[], &[]).unwrap();

        let mut journal = T1Journal::open(&rotation.new_path).unwrap();
        let receipt = journal.append_transaction(&tx_input(1)).unwrap();
        assert_eq!(receipt.id, first.id, "the same identity recognized");
        drop(journal);
        let scan = T1Journal::scan_file(&rotation.new_path).unwrap();
        assert_eq!(scan.committed.len(), 1, "no duplicate frames");
    }

    #[test]
    fn an_absent_pointer_resolves_the_legacy_path() {
        // The pre-rotation compatibility: no pointer — the legacy
        // journal itself is the active journal.
        let dir = TestDir::new("absent");
        let legacy = dir.journal();
        assert_eq!(active_journal_path(&legacy).unwrap(), legacy);
    }

    #[test]
    fn a_corrupt_pointer_fails_closed() {
        // An ambiguous active journal is never guessed: an empty
        // name, a traversal attempt, a separator, or an oversized
        // pointer each fail closed.
        let dir = TestDir::new("corrupt");
        let legacy = dir.journal();
        for content in [
            &b""[..],
            b"..",
            b"../evil",
            b"sub/dir/journal.t1",
            &[b'x'; 256][..],
        ] {
            fs::write(pointer_path(&legacy), content).unwrap();
            assert!(
                active_journal_path(&legacy).is_err(),
                "the pointer {:?} must fail closed",
                String::from_utf8_lossy(content)
            );
        }
    }
}
