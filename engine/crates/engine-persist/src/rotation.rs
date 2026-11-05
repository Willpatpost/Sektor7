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

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use engine_core::{Decoder, Encoder, SaveError};

use crate::journal::{
    CommittedTransaction, JournalError, RecordedCancellation, T1Journal, TailState, TransactionId,
    TransactionInput, io_err, sync_parent_dir,
};

/// The active-path pointer's suffix (beside the legacy journal).
pub const JOURNAL_ACTIVE_SUFFIX: &str = ".active";

/// The pointer's bounded content (a file name).
const MAX_ACTIVE_NAME_BYTES: u64 = 255;

/// The pointer flip's durability state (the review corrective R5):
/// the rename is atomic and the LIVE authority is the new journal,
/// but the flip is POWER-LOSS-DURABLE only once the pointer
/// directory sync completes. An unresolved state is NEVER a durably
/// completed transition: the caller fails closed (no further
/// authoritative T1 writes — the pre-rotation journal is a superset
/// only of the evidence existing at rotation time, never of future
/// writes to the new journal) until the durability is re-established
/// ([`reestablish_pointer_durability`]) or a verified recovery
/// repairs it. On platforms where std exposes no directory flush
/// (Windows) the sync is the ratified no-op and this state is
/// unreachable through natural failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerDurability {
    /// The pointer flip is durably established (the directory sync
    /// completed).
    Established,
    /// The rename succeeded (the live authority IS the new journal)
    /// but the directory sync failed: the flip may revert under
    /// power loss. Carries the failure detail; never a durably
    /// completed transition.
    Unresolved { detail: String },
}

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
    /// The retired-by-CANCELLATION identities (PP-B4 checkpoint 4):
    /// the durably canceled transactions — inert by construction,
    /// their cancellation frames the surviving evidence. Populated
    /// by the DRIVER's classifier (the mechanism itself cannot
    /// classify); the flat `retired` set is this list unioned with
    /// [`Self::retired_subsumed`].
    pub retired_canceled: Vec<TransactionId>,
    /// The retired-by-CONFIRMED-AND-SUBSUMED identities: every
    /// consequential identity canonically confirmed AND materialized
    /// below the verified anchor's lane boundary. Populated by the
    /// DRIVER's classifier; the two retirement classes stay DISTINCT
    /// evidence (a canceled retirement needs no anchor; a subsumed
    /// retirement is anchor-verified).
    pub retired_subsumed: Vec<TransactionId>,
    /// The cancellation records carried over (each surviving only
    /// while its target survives).
    pub retained_cancellations: u64,
    /// THE POINTER FLIP'S DURABILITY STATE (the review correctives
    /// R4 + R5): see [`PointerDurability`]. An `Unresolved` outcome
    /// is never a durably completed transition — the driver fails
    /// closed until the durability is re-established.
    pub pointer_durability: PointerDurability,
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

/// Re-establishes the pointer's power-loss durability (the review
/// corrective R5): the directory sync retried on demand — the
/// active-journal authority is whatever the pointer currently names
/// (correct in the live filesystem); this makes that choice durable.
/// The recovery completes this verification before any progression
/// (a fresh recovery never silently clears an unresolved
/// durability-uncertain condition merely by reading the visible
/// pointer); the durability-unresolved crossing exposes it as the
/// repair path. On platforms where std exposes no directory flush
/// (Windows) the sync is the ratified no-op.
pub fn reestablish_pointer_durability(root: &Path) -> Result<(), JournalError> {
    if POINTER_DIR_SYNC_FAULT.load(Ordering::Relaxed) {
        return Err(io_err(
            "re-sync journal dir (injected fault)",
            root,
            std::io::Error::other("injected pointer durability failure"),
        ));
    }
    let pointer = pointer_path(root);
    sync_parent_dir(&pointer).map_err(|e| io_err("re-sync journal dir", root, e))
}

/// The pointer file's path (beside the legacy journal).
fn pointer_path(legacy_path: &Path) -> PathBuf {
    let mut name = legacy_path.file_name().unwrap_or_default().to_os_string();
    name.push(JOURNAL_ACTIVE_SUFFIX);
    legacy_path.with_file_name(name)
}

/// The campaign's journal-generation ownership record's suffix
/// (beside the root — the review corrective R3).
pub const JOURNAL_GENERATIONS_SUFFIX: &str = ".generations";

/// The ownership record's versioned domain.
pub const JOURNAL_GENERATIONS_DOMAIN: &str = "engine-persist/journal-generations/1";
pub const JOURNAL_GENERATIONS_VERSION: u32 = 1;

/// Documented maxima (bounded decode).
pub const MAX_JOURNAL_GENERATIONS: u64 = 1 << 16;
pub const MAX_GENERATION_NAME_BYTES: u64 = 255;
/// The ownership record's whole-file bound (the read allocates only
/// under it).
pub const MAX_JOURNAL_GENERATIONS_FILE_BYTES: u64 = 1 << 20;

/// The ownership record's path (beside the root).
fn generations_path(legacy_path: &Path) -> PathBuf {
    let mut name = legacy_path.file_name().unwrap_or_default().to_os_string();
    name.push(JOURNAL_GENERATIONS_SUFFIX);
    legacy_path.with_file_name(name)
}

/// Test-only (the review corrective R4's deterministic fault
/// injection): when armed, the ownership record's read and write
/// return a real I/O error — the PRE-FLIP failure path (with the
/// record maintained before the pointer flip, the error returns
/// with the authority unchanged). Engine code never arms this; the
/// static is process-local.
#[doc(hidden)]
pub static GENERATIONS_IO_FAULT: AtomicBool = AtomicBool::new(false);

/// Test-only (the review corrective R4's deterministic fault
/// injection): when armed, the pointer directory sync after the
/// flip is skipped and reported as the rotation report's
/// durability warning — the POST-FLIP degraded-durability path
/// (never an error return: the live authority is already the new
/// journal). Engine code never arms this; the static is
/// process-local.
#[doc(hidden)]
pub static POINTER_DIR_SYNC_FAULT: AtomicBool = AtomicBool::new(false);

/// THE CAMPAIGN'S JOURNAL-GENERATION OWNERSHIP RECORD (the review
/// corrective R3): the generation names THIS campaign's rotations
/// have created or rotated away from — the retirement authority the
/// reclamation trusts, so a shared directory's foreign
/// `journal-*.t1` files (matching the filename predicate) are never
/// deletion candidates. Maintained by the rotation (atomic
/// tmp+fsync+rename); a missing record means no generation is
/// eligible (ownership conservatively unproven — a pre-rotation
/// campaign has no stale generations; a generation unrecorded by a
/// crash between the flip and the record's write is retained, and
/// the next rotation records it as its prior active).
pub fn read_journal_generations(root: &Path) -> Result<Vec<String>, JournalError> {
    if GENERATIONS_IO_FAULT.load(Ordering::Relaxed) {
        return Err(io_err(
            "read journal generations (injected fault)",
            &generations_path(root),
            std::io::Error::other("injected ownership-record I/O failure"),
        ));
    }
    let manifest = generations_path(root);
    match fs::metadata(&manifest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err("stat journal generations", &manifest, e)),
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(JournalError::Format(SaveError::Integrity(
                    "journal-generations/not-a-file",
                )));
            }
            if metadata.len() > MAX_JOURNAL_GENERATIONS_FILE_BYTES {
                return Err(JournalError::Format(SaveError::Limit {
                    section: "journal-generations",
                    kind: "file-length",
                    value: metadata.len(),
                    max: MAX_JOURNAL_GENERATIONS_FILE_BYTES,
                }));
            }
        }
    }
    let bytes =
        fs::read(&manifest).map_err(|e| io_err("read journal generations", &manifest, e))?;
    decode_generations(&bytes)
}

/// Removes the retired names from the ownership record (the
/// reclamation's journal retirement calls this after the deletions
/// — dead entries are inert either way, the record stays honest).
pub fn retire_journal_generations(root: &Path, retired: &[String]) -> Result<(), JournalError> {
    let names = read_journal_generations(root)?;
    if names.is_empty() {
        return Ok(());
    }
    let retired_set: BTreeSet<&String> = retired.iter().collect();
    let remaining: Vec<String> = names
        .into_iter()
        .filter(|name| !retired_set.contains(name))
        .collect();
    write_journal_generations(root, &remaining)
}

/// Encodes the ownership record (the versioned domain, the bounded
/// count, the plain file names).
fn encode_generations(names: &[String]) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(JOURNAL_GENERATIONS_DOMAIN);
    enc.write_u32(JOURNAL_GENERATIONS_VERSION);
    enc.write_count(names.len());
    for name in names {
        enc.write_str(name);
    }
    enc.into_bytes()
}

/// Strict decode: the domain and version gates, the bounded count,
/// the plain-file-name rule (never empty, never a traversal or
/// separator form), the duplicate rejection, no trailing bytes.
fn decode_generations(bytes: &[u8]) -> Result<Vec<String>, JournalError> {
    const SECTION: &str = "journal-generations";
    let mut dec = Decoder::new(bytes);
    dec.read_domain(JOURNAL_GENERATIONS_DOMAIN)
        .map_err(JournalError::Format)?;
    let version = dec.read_u32().map_err(JournalError::Format)?;
    if version != JOURNAL_GENERATIONS_VERSION {
        return Err(JournalError::Format(SaveError::Version {
            observed: version,
            supported: JOURNAL_GENERATIONS_VERSION,
        }));
    }
    let count = dec
        .read_count(MAX_JOURNAL_GENERATIONS, SECTION, "count")
        .map_err(JournalError::Format)?;
    let mut names = Vec::with_capacity(count as usize);
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        let name = dec
            .read_str(MAX_GENERATION_NAME_BYTES, SECTION)
            .map_err(JournalError::Format)?;
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(JournalError::Format(SaveError::Integrity(
                "journal-generations/name",
            )));
        }
        if !seen.insert(name.to_owned()) {
            return Err(JournalError::Format(SaveError::Integrity(
                "journal-generations/duplicate",
            )));
        }
        names.push(name.to_owned());
    }
    dec.finish().map_err(JournalError::Format)?;
    Ok(names)
}

/// Writes the ownership record atomically (tmp + fsync + rename +
/// the directory sync — the pointer's discipline).
fn write_journal_generations(root: &Path, names: &[String]) -> Result<(), JournalError> {
    if GENERATIONS_IO_FAULT.load(Ordering::Relaxed) {
        return Err(io_err(
            "write journal generations (injected fault)",
            &generations_path(root),
            std::io::Error::other("injected ownership-record I/O failure"),
        ));
    }
    let manifest = generations_path(root);
    let tmp = root.with_file_name(format!(
        "{}{JOURNAL_GENERATIONS_SUFFIX}.tmp",
        root.file_name().unwrap_or_default().to_string_lossy()
    ));
    let bytes = encode_generations(names);
    let mut file =
        File::create(&tmp).map_err(|e| io_err("create journal generations tmp", &tmp, e))?;
    file.write_all(&bytes)
        .map_err(|e| io_err("write journal generations tmp", &tmp, e))?;
    file.sync_all()
        .map_err(|e| io_err("sync journal generations tmp", &tmp, e))?;
    drop(file);
    fs::rename(&tmp, &manifest).map_err(|e| io_err("write journal generations", &manifest, e))?;
    sync_parent_dir(&manifest).map_err(|e| io_err("sync journal dir", &manifest, e))?;
    Ok(())
}

/// (F4-R11-B) THE SESSION-BOUND ROTATION — the only public rotation
/// entry: the authority derives from the verified session's own
/// pipeline (the live active-journal handle with its sidecar lock —
/// the lock ownership covers the transition — and the canonical
/// medium the pipeline verified), never from caller-supplied
/// retention evidence or bound identities. The bound arm is PROBED
/// through the pipeline's medium (an installed binding selects the
/// identity-inheriting successor; the authenticated absence keeps
/// the legacy arm — a bound root can never take the legacy arm, and
/// a legacy root never takes the bound arm); the classification is
/// DERIVED from the pipeline's own journal and confirmed set (§4.4:
/// retire iff canceled, or actionable with every identity confirmed
/// AND subsumed below the anchor boundary — the anchor boundary is
/// the trusted session's chain-verified claim, passed by the
/// crossing from the latest published snapshot); the mechanism is
/// the accepted crate-private sequence (write + scan-verify + the
/// pointer flip at the campaign root + the ownership record + the
/// durability rules, all unchanged).
pub fn rotate_session_journal<M: crate::medium::CanonicalMedium>(
    pipeline: &mut crate::pipeline::CommitPipeline<M>,
    root: &Path,
) -> Result<JournalRotation, SessionRotationError> {
    // (F4-R1/R11-B) THE BOUND-IDENTITY PROBE — in-crate, through the
    // pipeline's own medium (never caller-supplied).
    let bound_identities = {
        let medium = pipeline.medium_mut();
        let founding = crate::reclaim::founding_identity_medium(medium)?;
        crate::reclaim::find_installed_journal_root_binding(medium, &founding)?
            .map(|(_id, binding)| (binding.founding, binding.root_id))
    };
    // (R11-B) THE JOURNAL/ROOT AGREEMENT — before any successor is
    // created: the pipeline's journal must BE the authoritative
    // pointer-resolved ACTIVE journal at the requested root (an
    // arbitrary pipeline constructed with a journal from another
    // root, or a stale journal whose path differs from the resolved
    // active, refuses). The pipeline's live journal handle carries
    // the active's sidecar lock — the lock ownership covers the
    // transition — and the mechanism's conditioned root lock
    // establishes the root-side serialization boundary for the
    // ownership record and the pointer flip.
    let active = crate::rotation::active_journal_path(root)?;
    if pipeline.journal().path() != active {
        return Err(SessionRotationError::Journal(JournalError::CorruptHeader {
            detail: "the pipeline's journal is not the pointer-resolved active \
                     at the requested root — the rotation refuses"
                .into(),
        }));
    }
    if let Some((founding, root_id)) = bound_identities.as_ref() {
        // (R11-B) THE BOUND AGREEMENTS: the canonical binding's
        // location agrees with the requested root, and the pipeline's
        // journal carries the EXACT installed bound identities (the
        // active is the bound journal this binding names).
        let location = crate::reclaim::JournalRootBinding::normalize_location(root);
        let medium = pipeline.medium_mut();
        let installed = crate::reclaim::find_installed_journal_root_binding(medium, founding)?
            .expect("the probe above found the binding");
        if installed.1.location != location {
            return Err(SessionRotationError::Medium(
                crate::medium::MediumError::Chain {
                    rule: "journal-root/location-mismatch",
                },
            ));
        }
        crate::journal::scan_bound_file(&active, founding, root_id)
            .map_err(SessionRotationError::Journal)?;
    }
    // (THE AUTHENTICATED RETENTION BOUNDARY) The per-region
    // retirement boundary derives EXCLUSIVELY from the newest
    // canonical session snapshot — selected from the chain (the
    // HEAD-descent, the same selection rule as the recovery),
    // hash-verified through the medium's read, its domain exactly
    // the supported version, its director section strictly
    // validated (the shared session-anchor decoder — the ONE
    // interpretation of the wire format), and the boundary derived
    // from the decoded lane summary. NO caller-supplied boundary
    // exists in the API; a forged boundary is unrepresentable. No
    // snapshot on the chain refuses (no-anchor); an uncompacted
    // ledger (no summary) retires only what is independently
    // provable (cancellations); a malformed or
    // newer-unsupported-version snapshot fails closed.
    let anchor = crate::session_anchor::latest_session_anchor(pipeline.medium_mut())
        .map_err(SessionRotationError::Anchor)
        .and_then(|anchor| {
            anchor.ok_or(SessionRotationError::Anchor(
                crate::session_anchor::AnchorError::NoAnchor,
            ))
        })?;
    let anchor_boundary = anchor.boundary;
    // THE CLASSIFICATION (§4.4) — derived from the pipeline's own
    // journal (the live active handle) and confirmed set, under the
    // authenticated boundary.
    let journal = pipeline.journal();
    let mut retained: Vec<CommittedTransaction> = Vec::new();
    let mut retired_canceled: Vec<TransactionId> = Vec::new();
    let mut retired_subsumed: Vec<TransactionId> = Vec::new();
    for transaction in journal.transactions() {
        let canceled = journal.is_canceled(transaction.id);
        let settled = transaction.envelopes.iter().all(|envelope| {
            pipeline.is_confirmed(envelope.id)
                && anchor_boundary.is_some_and(|boundary| {
                    envelope.id.episode <= boundary[envelope.id.region as usize]
                })
        });
        if canceled {
            retired_canceled.push(transaction.id);
        } else if settled {
            retired_subsumed.push(transaction.id);
        } else {
            retained.push(transaction.clone());
        }
    }
    let retired: Vec<TransactionId> = retired_canceled
        .iter()
        .chain(retired_subsumed.iter())
        .copied()
        .collect();
    let retained_ids: std::collections::BTreeSet<TransactionId> =
        retained.iter().map(|transaction| transaction.id).collect();
    let retained_cancellations: Vec<RecordedCancellation> = journal
        .cancellations()
        .iter()
        .filter(|cancellation| retained_ids.contains(&cancellation.target))
        .cloned()
        .collect();
    let mut rotation = rotate_journal_with(
        root,
        &retained,
        &retained_cancellations,
        &retired,
        bound_identities.as_ref().map(|(f, r)| (f, r)),
    )?;
    // The classified retirement evidence (the driver-populated
    // report fields — the mechanism cannot classify).
    rotation.retired_canceled = retired_canceled;
    rotation.retired_subsumed = retired_subsumed;
    Ok(rotation)
}

/// (F4-R11-B) The session-bound rotation's typed error: the binding
/// probe's medium failures, the authenticated anchor's typed
/// failures, and the mechanism's journal failures, never conflated.
#[derive(Debug)]
pub enum SessionRotationError {
    Medium(crate::medium::MediumError),
    Anchor(crate::session_anchor::AnchorError),
    Journal(JournalError),
}

impl From<crate::medium::MediumError> for SessionRotationError {
    fn from(error: crate::medium::MediumError) -> Self {
        SessionRotationError::Medium(error)
    }
}

impl From<JournalError> for SessionRotationError {
    fn from(error: JournalError) -> Self {
        SessionRotationError::Journal(error)
    }
}

/// (V2-W F4) The rotation with the optional bound identity: when
/// present, the fresh generation carries the SAME canonical founding
/// hash and stable journal-root ID (the identities inherit across
/// every generation; the verification scans the bound form, refusing
/// mixed, corrupt, or missing identity evidence). The pointer, the
/// ownership record, and the durability semantics are the accepted
/// ones — the identical flip sequence. (F4-R11-B) CRATE-PRIVATE: the
/// raw mechanism is reachable only through
/// [`rotate_session_journal`] — the bound identities and the
/// retention classification are never external inputs.
pub(crate) fn rotate_journal_with(
    legacy_path: &Path,
    retained: &[CommittedTransaction],
    retained_cancellations: &[RecordedCancellation],
    retired: &[TransactionId],
    bound: Option<(&crate::hash::Hash, &crate::hash::Hash)>,
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
        let mut journal = match bound {
            Some((founding, root_id)) => {
                // (F4-R11) THE AUTHORIZED SUCCESSOR CREATION: the
                // rotation names a fresh nanos-unique generation and
                // the creating constructor refuses an existing path —
                // it is structurally unable to replace authoritative
                // evidence.
                T1Journal::create_bound_generation(&new_path, founding, root_id)?
            }
            None => T1Journal::open(&new_path)?,
        };
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
    let scan = match bound {
        Some((founding, root_id)) => crate::journal::scan_bound_file(&new_path, founding, root_id)?,
        None => T1Journal::scan_file(&new_path)?,
    };
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
    // (V2-W F4, §6) THE ROOT-SIDECAR EXCLUSIVE LOCK — the
    // serialization boundary against the bound plan publication
    // (which holds the SAME root lock across its journal-obligation
    // collection and the canonical plan append). The lock is taken
    // ONLY when the active journal is a ROTATED generation: in the
    // unrotated case the active IS the root, and the caller (the
    // crossing) already holds the root's own sidecar lock through
    // its pipeline journal handle for the ENTIRE rotation — the
    // publication's root lock is thereby blocked for the whole
    // interval (the same serialization, no same-process flock
    // self-deadlock). In the rotated case the caller holds only the
    // active generation's lock, so THIS function takes the root
    // lock across the ownership-record write and the pointer flip.
    // The rotation never takes the medium's HEAD lock and the
    // publication never takes the active generation's lock — no
    // cycle.
    let prior_active = active_journal_path(legacy_path)?;
    let _root_lock = if prior_active != legacy_path {
        Some(crate::journal::open_lock_file(legacy_path)?)
    } else {
        None
    };
    // (4) THE OWNERSHIP RECORD — BEFORE THE FLIP (the review
    // corrective R4): the campaign's generation lineage — the
    // pre-flip active and the new generation, deduplicated — is
    // maintained beside the root BEFORE the authoritative pointer
    // changes. The publication ordering makes every ownership-record
    // failure a PRE-FLIP failure: the error returns with the
    // authority unchanged (the caller stays safely on the current
    // journal — a manifest I/O failure can never hand control back
    // to an operational stale-journal writer, because the pointer
    // has not moved). A crash between this write and the flip
    // leaves the new generation recorded but inert — the pointer
    // still names the prior journal (the authority and a superset),
    // and a recorded-but-never-active generation is harmless.
    let name = new_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(JournalError::Format(SaveError::Integrity(
            "journal-rotation/name",
        )))?;
    let mut owned = read_journal_generations(legacy_path)?;
    if let Some(prior_name) = prior_active.file_name().and_then(|prior| prior.to_str())
        && !owned.iter().any(|recorded| recorded == prior_name)
    {
        owned.push(prior_name.to_owned());
    }
    if !owned.iter().any(|recorded| recorded == name) {
        owned.push(name.to_owned());
    }
    write_journal_generations(legacy_path, &owned)?;
    // (5) THE DURABLE ACTIVE-PATH FLIP: tmp + fsync + rename onto
    // the pointer — atomic, so a crash leaves either the
    // pre-rotation pointer state or the completed flip, never a
    // torn pointer. THE AUTHORITY BOUNDARY (the review corrective
    // R4): the rename is the last fallible step that may return an
    // error — after it, the live pointer names the new journal, so
    // no later failure may return an error that leaves the caller
    // operational on the stale journal. The directory sync that
    // follows is a DURABILITY step, not an authority step: its
    // failure means the flip may not survive a power loss — the
    // live authority is still the new journal — so it is carried
    // as the report's pointer_durability state (the review
    // corrective R5): the caller FAILS CLOSED on Unresolved — no
    // further authoritative T1 writes until the durability is
    // re-established — because the pre-rotation journal is a
    // superset only of the evidence existing at rotation time,
    // never of future writes to the new journal.
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
    let pointer_durability = if POINTER_DIR_SYNC_FAULT.load(Ordering::Relaxed) {
        PointerDurability::Unresolved {
            detail: "journal-rotation/pointer-dir-sync (injected fault)".to_owned(),
        }
    } else {
        match sync_parent_dir(&pointer_path(legacy_path)) {
            Ok(()) => PointerDurability::Established,
            Err(e) => PointerDurability::Unresolved {
                detail: io_err("sync journal dir", legacy_path, e).to_string(),
            },
        }
    };
    Ok(JournalRotation {
        new_path,
        legacy_path: legacy_path.to_path_buf(),
        retained: retained.iter().map(|transaction| transaction.id).collect(),
        retired: retired.to_vec(),
        // The classified retirement evidence is DRIVER-populated (the
        // mechanism cannot classify) — see the field docs.
        retired_canceled: Vec::new(),
        retired_subsumed: Vec::new(),
        retained_cancellations: retained_cancellations.len() as u64,
        pointer_durability,
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

        let rotation = rotate_journal_with(&legacy, &survivors, &[], &[second.id], None).unwrap();

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
        let rotation = rotate_journal_with(&legacy, &survivors, &[], &[], None).unwrap();

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
