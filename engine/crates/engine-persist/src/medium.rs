//! The canonical-medium boundary (§3.3, §4.2, §5.3.1): what a
//! conditional append can produce, what a typed failure is, and what
//! a completed publication sequence honestly establishes per
//! platform. The future CommitPipeline (B3) operates over this
//! boundary; B2 ships the local provider and the contract.

use std::fmt;

use engine_core::wire::MAX_TAG_LEN;
use engine_core::{Decoder, SaveError};

use crate::hash::{Hash, hash_object};
use crate::record::{AuthorityMode, RevisionRecord};

/// What a completed publication sequence establishes on this platform
/// (§5.3.1). The tier is attached to every `Accepted` outcome so a
/// caller can never claim broader durability than the result carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncTier {
    /// The full POSIX sequence — every object file fsynced, the
    /// objects directory fsynced, `HEAD.tmp` fsynced, renamed onto
    /// `HEAD`, and the root directory fsynced, each returning success
    /// — establishing acknowledged-revision persistence across
    /// process crash, OS crash, and power loss per the documented
    /// fsync contract. macOS note: std exposes no `F_FULLFSYNC`; the
    /// claim is the documented std semantics, never stronger.
    FullPosixSequence,
    /// The documented std Windows sequence (file fsync + rename
    /// ordering) completed — the **process-crash tier**.
    /// Acknowledged-revision persistence across OS crash or sudden
    /// power loss is **not claimed from std**: the N−1-valid/N-lost
    /// scenario of §5.3.3 is possible and undetectable by chain
    /// validation. Development / process-crash qualification only.
    ProcessCrashOnly,
    /// The verified SQLite transactional configuration (PP-B1,
    /// D-PP-1): WAL with `synchronous=FULL`, both **confirmed by
    /// querying the actual configuration at open** — every committed
    /// transaction's WAL frames are fsynced before the commit
    /// acknowledges, the documented acknowledged-transaction
    /// persistence contract across process crash, OS crash, and power
    /// loss per SQLite's documented WAL semantics (the
    /// hardware/filesystem synchronization assumptions stated in the
    /// PP-B1 record). Process-kill tests establish the process-crash
    /// behavior only; the broader tier rests on the documented engine
    /// semantics, never on tests runnable on development hardware.
    TransactionalWalFull,
}

/// Why an append outcome is uncertain. The outcome is `Unknown` —
/// never guessed into success or failure; reconciliation under the
/// lock classifies it (§5.3 step 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// Failure while writing an immutable object's bytes.
    ObjectWriteFailed,
    /// Failure while fsyncing an object file or the objects directory.
    ObjectSyncFailed,
    /// Failure writing or synchronizing the temporary HEAD, or the
    /// rename itself.
    HeadPublishFailed,
    /// Failure in post-rename metadata synchronization.
    PostPublishSyncFailed,
    /// A SQLite transaction's `COMMIT` could not be completed — the
    /// transaction's final disposition (installed or not) cannot be
    /// established from the returned error; reconcile under a fresh
    /// committed read (PP-B1).
    CommitUncertain,
    /// A failed SQLite transaction's `ROLLBACK` could not be
    /// completed — the disposition is genuinely unknown (PP-B1).
    RollbackUncertain,
    /// The canonical I/O worker's transport was lost around an
    /// append — the worker exited (or its reply was lost) after the
    /// job may have executed (PP-B4 checkpoint 2). The candidate's
    /// installation is uncertain: never a definite failure; the
    /// candidate's identity is retained for reconciliation under the
    /// lock (the pipeline's pending attempt, or a fresh recovery's
    /// chain walk).
    WorkerReplyLost,
}

/// The result of a conditional append (§5.3.1's exact semantics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendOutcome {
    /// The obligated publication and synchronization steps completed
    /// successfully, to the tier the outcome carries. `Accepted` means
    /// exactly "this platform's documented sequence completed" — never
    /// anything broader.
    Accepted { id: Hash, tier: SyncTier },
    /// Another revision won the CAS race. The observed authoritative
    /// HEAD is returned; the stale candidate was not installed and
    /// authoritative state was not altered.
    RejectedStaleParent { observed: Option<Hash> },
    /// The operation may or may not have installed, or its committed
    /// durability cannot be established. Reconcile under the lock; a
    /// retry must carry the same record bytes (hence the same content
    /// identity) so it can never duplicate a canonical effect.
    Unknown { reason: UnknownReason },
}

/// The classification a lock-held reconciliation produces (§5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The candidate is the authoritative HEAD, or an ancestor of it —
    /// its effect is in the chain; a retry of the same logical
    /// operation must not duplicate it.
    Installed { id: Hash },
    /// The candidate is not installed; HEAD is as observed. An
    /// idempotent retry is appropriate (the same record bytes produce
    /// the same identity).
    NotInstalled { head: Option<Hash> },
    /// Verification could not establish the result — uncertainty is
    /// retained, never converted into a confirmation.
    Unverifiable { reason: String },
}

/// Typed medium failures. None of these hide an uncertain commit — an
/// uncertain commit is the `Unknown` **outcome**, carried explicitly
/// and separately from errors.
#[derive(Debug)]
pub enum MediumError {
    /// Filesystem I/O, with the operation and path for context.
    Io {
        op: &'static str,
        path: String,
        source: std::io::Error,
    },
    /// A referenced object is absent from the store.
    MissingObject { hash: Hash },
    /// An object's bytes do not hash to the identity they are stored
    /// under.
    HashMismatch {
        expected: Hash,
        observed: Hash,
        context: &'static str,
    },
    /// The authoritative HEAD file is unparseable (truncated, torn, or
    /// trailing bytes) — fail closed; a valid previous prefix is never
    /// silently promoted.
    CorruptHead { detail: String },
    /// A chain or append validation violation; the rule names it:
    /// `epoch-regression`, `parent-mismatch`, `kind-incoherent`,
    /// `duplicate-idempotency-key`, `cycle`, `divergence`,
    /// `content-set-mismatch`, `depends-on-unresolvable` (a
    /// dependency must name a strictly earlier revision of the
    /// authoritative chain), `record-noncanonical`, `founding-missing`,
    /// `unfounded`.
    Chain { rule: &'static str },
    /// A chain-referenced object failed its hash — referenced
    /// corruption is fail-closed and evidence-preserving (never
    /// replaced, never deleted).
    ReferencedObjectCorrupt { hash: Hash },
    /// A persisted object failed strict decoding (the Batch 1 typed
    /// error carried through).
    Format(SaveError),
    /// Writer contention persisted past the configured busy timeout —
    /// the SQLite write lock was unavailable (PP-B1). **Never evidence
    /// of a stale parent** (no parent comparison occurred — no
    /// transaction work began) and **never an uncertain commit** (the
    /// failure precedes the transaction's first write).
    Contention {
        /// What was contended and where.
        detail: String,
    },
    /// A SQLite database-level failure (corruption, unreadable
    /// storage, an API-level error) — fail-closed, never automatically
    /// repaired (PP-B1). The `detail` carries the engine's message;
    /// the engine never attempts `.recover` or any destructive repair.
    Sqlite {
        /// The operation that failed.
        op: &'static str,
        /// The database engine's message.
        detail: String,
    },
    /// The local store directory carries a `MIGRATED` tombstone (the
    /// §6.1 offline migration's seal, PP-B1): it is archived
    /// evidence, not a live campaign source — new binaries refuse to
    /// open it. Binaries predating the tombstone cannot be made to
    /// honor it (the ratified documented operational limitation);
    /// the tombstone is evidence and is never deleted.
    Sealed {
        /// The refusing store's context.
        detail: String,
    },
    /// The SQLite database carries a `migration-pending` meta row
    /// (the §6.1 offline migration's target lifecycle — the PP-B1
    /// review corrective): it is a partially- or fully-copied
    /// migration target whose source-side seal and activation have
    /// not both completed. It is NOT a live campaign — the normal
    /// constructors refuse it; only the migration itself
    /// (`SqliteMedium::open_migration_target`) may proceed, and the
    /// activation (`mark_migrated_from`) transitions it to a live
    /// campaign transactionally.
    MigrationPending {
        /// The refusing database's context.
        detail: String,
    },
    /// The canonical I/O worker is unavailable (PP-B4 checkpoint 2):
    /// the queue's receiver is gone (the worker thread exited —
    /// normally, poisoned, or panicked) so the request was never
    /// admitted, or a side-effect-free read's reply was lost to the
    /// worker's death. DEFINITE for the request it names (an
    /// unadmitted job never ran; a read never installed anything) —
    /// and the session is poisoned: every authoritative operation
    /// refuses until a fresh recovery reestablishes trustworthy
    /// state. A possibly-executed APPEND never surfaces this error:
    /// it arrives as `AppendOutcome::Unknown` with
    /// `UnknownReason::WorkerReplyLost`.
    WorkerUnavailable {
        /// What failed and where.
        detail: String,
    },
    /// The dedicated composite admission refused the publication
    /// (K2-C1): a typed, identity-naming failure — the root
    /// manifest, the dependency set, or a chunk dependency could
    /// not be established. Nothing was mutated.
    CompositeAdmission {
        /// What was proven and refused.
        detail: String,
    },
    /// The generic append's snapshot-family domain gate refused a
    /// composite (or unrecognized future family) snapshot root
    /// (K2-C1): such objects may enter ONLY through the dedicated
    /// `append_composite` admission.
    CompositeDomainGate {
        /// The refused object's leading domain.
        domain: String,
    },
}

impl fmt::Display for MediumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediumError::Io { op, path, source } => {
                write!(f, "storage I/O failure during {op} at {path}: {source}")
            }
            MediumError::MissingObject { hash } => {
                write!(f, "referenced object {hash} is missing from the store")
            }
            MediumError::HashMismatch {
                expected,
                observed,
                context,
            } => write!(
                f,
                "object hash mismatch in {context}: expected {expected}, observed {observed}"
            ),
            MediumError::CorruptHead { detail } => {
                write!(f, "the authoritative HEAD is corrupt: {detail}")
            }
            MediumError::Chain { rule } => write!(f, "chain validation failed: {rule}"),
            MediumError::ReferencedObjectCorrupt { hash } => write!(
                f,
                "chain-referenced object {hash} fails its hash — fail closed, evidence preserved"
            ),
            MediumError::Format(error) => write!(f, "persisted object failed decoding: {error}"),
            MediumError::Contention { detail } => {
                write!(f, "storage write contention: {detail}")
            }
            MediumError::Sqlite { op, detail } => {
                write!(f, "sqlite operation {op} failed: {detail}")
            }
            MediumError::Sealed { detail } => {
                write!(f, "the store is sealed as migrated: {detail}")
            }
            MediumError::MigrationPending { detail } => {
                write!(
                    f,
                    "the database is an unactivated migration target: {detail}"
                )
            }
            MediumError::WorkerUnavailable { detail } => {
                write!(f, "the canonical I/O worker is unavailable: {detail}")
            }
            MediumError::CompositeAdmission { detail } => {
                write!(f, "the composite admission refused: {detail}")
            }
            MediumError::CompositeDomainGate { domain } => {
                write!(
                    f,
                    "the generic append refuses the snapshot-family domain {domain} — use the composite admission"
                )
            }
        }
    }
}

impl std::error::Error for MediumError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MediumError::Io { source, .. } => Some(source),
            MediumError::Format(error) => Some(error),
            _ => None,
        }
    }
}

/// A canonical immutable object supplied to an append: bytes that
/// begin with a valid length-prefixed domain tag. A Batch 1 save
/// (`sektor7/save/1`) is one such domain — snapshots are saves; the
/// store is domain-agnostic and verifies only integrity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    bytes: Vec<u8>,
    domain: String,
}

impl StoredObject {
    /// Validates that the bytes begin with a well-formed domain tag
    /// (the payload is not parsed here — object-type interpretation
    /// belongs to the reader of that domain).
    pub fn new(bytes: Vec<u8>) -> Result<Self, MediumError> {
        let mut dec = Decoder::new(&bytes);
        let domain = dec
            .read_str(MAX_TAG_LEN, "stored-object")
            .map_err(MediumError::Format)?
            .to_owned();
        Ok(StoredObject { bytes, domain })
    }

    /// The object's domain tag.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// The raw canonical bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The object's content identity.
    pub fn hash(&self) -> Hash {
        hash_object(&self.bytes)
    }

    /// Consumes the object, returning its bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// What a cold validation established about the authoritative chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainSummary {
    pub head: Hash,
    /// The head's epoch (the chain height − 1; the founding record is
    /// epoch 0).
    pub height: u64,
    pub founding: Hash,
    pub authority: AuthorityMode,
    pub campaign_id: String,
    /// Records walked, founding included.
    pub records: u64,
    /// Distinct content objects referenced across the chain.
    pub content_objects: u64,
    /// The checkpoint-metadata revisions (MKs) on the chain, in walk
    /// order (newest first) — the permanent anchors (PP-B3). Empty
    /// before any checkpoint exists.
    pub checkpoints: Vec<Hash>,
    /// The reclaim-indexed records across every on-chain MK's index
    /// whose revision objects are genuinely absent (0 before any
    /// reclamation — an index whose records are still present is
    /// evidence for their FUTURE reclamation, not reclaimed state).
    pub reclaimed_records: u64,
    /// The idempotency keys carried by those genuinely-absent index
    /// entries (keys are chain-unique, so this counts distinct keys)
    /// — the historical membership evidence (PP-B3's mandatory
    /// condition 2). 0 before any reclamation.
    pub reclaimed_keys: u64,
}

/// The verified classification of a stored content object: the
/// leading domain tag and the total byte length, established under
/// the same full-object hash verification as
/// [`CanonicalMedium::read_object`] (the store itself remains
/// domain-agnostic — the tag is an observational property of the
/// encoded content, not trusted metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectClass {
    /// The object's leading domain tag, when the leading bytes decode
    /// as one (`None` for a non-decoding prefix — intact content with
    /// no recognizable tag).
    pub domain: Option<String>,
    /// The object's total byte length (an honest size witness for
    /// read-amplification accounting).
    pub length: u64,
}

impl ObjectClass {
    /// Classifies intact, already-verified content.
    pub fn of(bytes: &[u8]) -> Self {
        let domain = Decoder::new(bytes)
            .read_str(MAX_TAG_LEN, "content-peek")
            .ok()
            .map(str::to_owned);
        ObjectClass {
            domain,
            length: bytes.len() as u64,
        }
    }
}

/// The storage boundary the future CommitPipeline will operate over
/// (§4.2's contract, typed). Every operation — including reads —
/// serializes on the medium's OS-managed coordination, so a concurrent
/// sweep can never race a read.
/// The VERIFIED publication receipt (the K2-C1 corrective's
/// unforgeable admission boundary): the record and the object set a
/// provider may install, together with the kind of verification
/// that produced it. **Public type, PRIVATE fields, in-crate-only
/// constructors** — the generic form is created by the provided
/// [`CanonicalMedium::append`] after the snapshot-family gate; the
/// composite form is created by the provided
/// [`CanonicalMedium::append_composite`] after the full composite
/// admission. External code (including `engine-slice`'s worker
/// transport and store dispatch) can hold, pass, and consume the
/// value BY VALUE but can never construct, mutate, or reinterpret
/// one — every externally reachable installing method requires
/// either passing the fresh gate or holding this receipt. The
/// receipt carries no mutable access to its validated content and
/// grants no exemption from the provider's CAS and idempotency
/// checks.
#[derive(Debug, Clone)]
pub struct AdmittedAppend {
    record: RevisionRecord,
    objects: Vec<StoredObject>,
    /// Whether the object set exceeds the record's direct
    /// references because the composite admission verified the
    /// extras against the root manifest (the root-only record
    /// contract).
    composite: bool,
}

impl AdmittedAppend {
    /// The verified GENERIC form — in-crate only (the provided
    /// `append` after the domain gate).
    pub(crate) fn generic(record: &RevisionRecord, objects: &[StoredObject]) -> Self {
        AdmittedAppend {
            record: record.clone(),
            objects: objects.to_vec(),
            composite: false,
        }
    }

    /// The verified COMPOSITE form — in-crate only (the provided
    /// `append_composite` after the full admission): the root-only
    /// record and the root plus new-chunk object set.
    pub(crate) fn composite(
        record: &RevisionRecord,
        root: &StoredObject,
        new_chunks: &[StoredObject],
    ) -> Self {
        let mut objects = Vec::with_capacity(new_chunks.len() + 1);
        objects.push(root.clone());
        objects.extend(new_chunks.iter().cloned());
        AdmittedAppend {
            record: record.clone(),
            objects,
            composite: true,
        }
    }

    /// The provider-side view (in-crate only): the record, the
    /// object set, and the kind. Consumed by the providers'
    /// `append_admitted` implementations and the receipt's own
    /// Clone — a replayed receipt still passes the provider's full
    /// CAS/idempotency checks, so a duplicate can never install a
    /// duplicate canonical effect.
    pub(crate) fn parts(&self) -> (&RevisionRecord, &[StoredObject], bool) {
        (&self.record, &self.objects, self.composite)
    }

    /// The READ-ONLY test accessor (the race fixtures and the
    /// verification tests observe the receipt's content — no
    /// construction, no mutation; the unforgeability is unchanged).
    #[doc(hidden)]
    pub fn parts_for_tests(&self) -> (&RevisionRecord, &[StoredObject], bool) {
        self.parts()
    }
}

pub trait CanonicalMedium {
    /// The authoritative HEAD revision, or `None` for an unfounded
    /// campaign.
    fn head(&mut self) -> Result<Option<Hash>, MediumError>;

    /// Conditionally appends `record` — with exactly the content
    /// objects `record.content_refs` names — against
    /// `expected_parent` (`None` founds the campaign). Exactly one
    /// concurrent writer for a given expected parent can be
    /// `Accepted`; losers observe `RejectedStaleParent`; any failure
    /// from the first object write onward is `Unknown`.
    /// The GENERIC publication. Provided (K2-C1 corrective): the
    /// snapshot-family domain gate, then the VERIFIED publication
    /// receipt (an [`AdmittedAppend`] — constructible only inside
    /// engine-persist after the gate and the structural checks),
    /// then the provider dispatch. A composite (or unrecognized
    /// future family) snapshot root may enter ONLY through the
    /// dedicated [`Self::append_composite`] admission; the legacy
    /// `/1` path is unchanged. There is NO raw-publication method
    /// on this trait — every externally reachable installing method
    /// requires either passing this gate or holding an unforgeable
    /// admission receipt.
    fn append(
        &mut self,
        record: &RevisionRecord,
        objects: &[StoredObject],
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        crate::composite_append::enforce_snapshot_family_gate(objects)?;
        let admitted = AdmittedAppend::generic(record, objects);
        self.append_admitted(admitted, expected_parent)
    }

    /// The DEDICATED composite publication (K2-C1 corrective): the
    /// fail-closed admission — the strict root decode, the
    /// ROOT-ONLY record binding (the ratified K2-A contract:
    /// `content_refs == [root_identity]`), the exact dependency
    /// set, and EVERY chunk fully verified (the new against their
    /// supplied bytes, the reused through the medium's hash-verified
    /// read) — then the verified publication receipt and the
    /// provider dispatch, carrying the root plus the new chunks as
    /// the stored object set while the RECORD references only the
    /// root (the chunks are transitively referenced through the
    /// manifest). Every admission failure refuses typed BEFORE any
    /// mutation; the CAS, idempotency, and Unknown/reconciliation
    /// guarantees are the provider's existing ones.
    ///
    /// **The K2-C2 activation barrier:** composite publication is
    /// NOT production-available — the reused-dependency versus
    /// reclamation race is unresolved until K2-C2's reservation
    /// fence. The admission refuses unless the explicit TEST-ONLY
    /// activation has been set
    /// ([`crate::composite_append::test_enable_composite_publication`]
    /// — never called by any production path); the refusal is the
    /// enforceable barrier, not a convention.
    fn append_composite(
        &mut self,
        request: &crate::composite_append::CompositeAppend,
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        let admitted = crate::composite_append::admit_for_publication(request, |identity| {
            self.read_object(identity)
        })
        .map_err(crate::composite_append::CompositeAdmissionError::into_medium_error)?;
        self.append_admitted(admitted, expected_parent)
    }

    /// The verified publication dispatch every provider implements.
    /// The [`AdmittedAppend`] argument is UNFORGEABLE: a public type
    /// with private fields and an in-crate-only constructor —
    /// `engine-slice` (the worker transport, the store dispatch)
    /// can hold, pass, and consume it BY VALUE but can never
    /// construct, mutate, or reinterpret one. The provider runs its
    /// own structural checks (the canonical record decode; the set
    /// rule by kind — the generic form's supplied==referenced
    /// equality, the composite form's root-only record with the
    /// admission-verified extras) and then its established
    /// publication sequence with all of its CAS, idempotency,
    /// Unknown, and reconciliation semantics.
    fn append_admitted(
        &mut self,
        admitted: AdmittedAppend,
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError>;

    /// Reads, hash-verifies, and decodes a revision record by
    /// identity.
    fn read_record(&mut self, id: &Hash) -> Result<RevisionRecord, MediumError>;

    /// Reads and hash-verifies a content object by identity.
    fn read_object(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError>;

    /// Classifies a content object under COMPLETE hash verification:
    /// the full object bytes are read and hash-checked against the
    /// requested identity exactly as [`CanonicalMedium::read_object`]
    /// does — a missing object fails `MissingObject`, a corrupted
    /// object fails `HashMismatch`, and a classification result is
    /// only returned for intact content. The default implementation
    /// delegates to `read_object` (every existing implementation is
    /// source-compatible); a provider MAY perform the verified read
    /// internally and return only the small [`ObjectClass`], which
    /// eliminates the byte transfer while preserving the
    /// verification. Never a prefix-only read.
    fn classify_object(&mut self, hash: &Hash) -> Result<ObjectClass, MediumError> {
        let bytes = self.read_object(hash)?;
        Ok(ObjectClass::of(&bytes))
    }

    /// Cold, complete validation of the authoritative chain — the
    /// session-open walk (founding → HEAD, every record and every
    /// referenced object hash-verified).
    fn validate(&mut self) -> Result<ChainSummary, MediumError>;

    /// Reconciles an ambiguous (`Unknown`) append under the lock —
    /// never fabricates a confirmation.
    fn reconcile_unknown(&mut self, candidate: &Hash) -> Result<Reconciliation, MediumError>;

    /// What `Accepted` establishes on this platform.
    fn durability_tier(&self) -> SyncTier;

    /// The explicit reclamation delete (PP-B3's Phase B, the review
    /// corrective F4): remove the AUDITED units — each record object
    /// and its referenced content objects, from the reclamation
    /// ledger's manifest, never a re-read of a possibly-absent
    /// record — IDEMPOTENTLY (an absent row or file is already
    /// reclaimed, never an error; a re-run deletes only what
    /// remains, and an interrupted unit's orphaned objects are
    /// discoverable from the manifest). The eligibility was verified
    /// and audited BEFORE this call (Phase A) — the delete trusts
    /// the audit and never rewrites bytes. A half-deleted unit
    /// referenced by a RETAINED record fails closed at the next
    /// validation (the existing MissingObject). Returns the count of
    /// objects removed by this call.
    fn reclaim(&mut self, units: &[crate::reclaim::ReclaimUnit]) -> Result<u64, MediumError>;

    /// The manifest-completion probe (PP-B3's Phase B, the review
    /// corrective R1): does ANY audited byte remain — a unit's
    /// record object, a referenced content object, or a copy the
    /// orphan sweep quarantined? A reclamation's completion is
    /// judged against the WHOLE audited manifest, never record
    /// presence alone: an interrupted Phase B that removed every
    /// record file but not yet the objects (or the sweep-moved
    /// copies) is RESTARTABLE WORK, not a completed reclamation.
    /// Read-only; the same manifest [`Self::reclaim`] deletes from.
    fn reclaim_pending(
        &mut self,
        units: &[crate::reclaim::ReclaimUnit],
    ) -> Result<bool, MediumError>;

    /// The graceful lifecycle boundary (PP-B1's ratified provider
    /// policy, joined to the trait at PP-B4 checkpoint 1 so the
    /// crossing's medium-level checkpoint routes through the same
    /// injected medium as every other canonical access): the SQLite
    /// provider's WAL `TRUNCATE` checkpoint — a NO-OP by default for
    /// every medium whose acknowledged publications already fsync
    /// through their own append contract (the file medium's
    /// nine-step). Failures are typed, never swallowed.
    fn checkpoint(&mut self) -> Result<(), MediumError> {
        Ok(())
    }
}
