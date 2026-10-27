//! PP-B4 checkpoint 3: the orchestrated authoritative session — the
//! Revision-6 preflight's narrow owner. [`OrchestratedSession`]
//! encapsulates the `Spine`, the `RegionDirector`, the worker-backed
//! [`SessionCrossing`], and the [`IoWorker`]: the presence-driven
//! authoritative tick (the composed gate first; the exclusive
//! transition routing — live→dormant through the six-phase durable
//! crossing, dormant→live through the durable reactivation,
//! live→live through the accepted safe path; `run_tick` only after
//! every required durable transition succeeded), the SEPARATE
//! authority-gated `schedule_event` (the accepted region-aware
//! dispatch verbatim — never a reimplementation), and the
//! asynchronous snapshot publication (the capture/submit split).
//!
//! **The publication discipline (the accepted preflight):** a capture
//! is an IMMUTABLE record — the serialized session bytes, the observed
//! canonical parent, the content identity, the tick, the STRUCTURED
//! consequential coverage ([`CoverageFrontier`] — never an opaque
//! content hash), the journal frontier (the committed-transaction
//! identity set), and the capture-validity generation (the journal
//! flip count). Admission is NONBLOCKING (`try_submit` through the
//! single worker): a full or unavailable queue returns the typed
//! [`SubmitRefusal`] with NO canonical append; a waiting REQUIRED
//! operation (a confirmation) sets the fairness gate so continued
//! optional submissions can never starve it. Once admitted, the
//! publication executes FIFO against the CAPTURED parent — a HEAD
//! advance surfaces the stale rejection and the bytes are NEVER
//! rebased; cancellation is effective only before the job starts (a
//! job already executing runs to its completion boundary — its
//! possibly-committed outcome is tracked, never erased). No
//! completion reports a snapshot safely saved merely because the job
//! was queued; an unknown disposition is reported as unknown, never
//! interpreted. Worker death poisons authoritative progression
//! through the composed gate — without anyone polling a completion.
//! Every completion RETAINS its typed disposition in the in-session
//! completion ledger ([`CompletionRecord`]) — the one-shot poll/wait
//! result and the durable-in-session record are distinct surfaces,
//! and the authority-side reconciliation reads the ledger after the
//! caller consumed the outcome; the pending slots and the ledger
//! grow unboundedly over the session's lifetime by design (a bounded
//! queue bounds ADMISSION, never the accounting — unresolved
//! obligations are never silently discarded), and
//! [`OrchestratedSession::join`] harvests the unpolled slots exactly
//! as the maintenance drain does.
//!
//! THE PROTECTED MAINTENANCE LEASE (PP-B4 checkpoint 4): every
//! maintenance compound — the SK/MK checkpoint, the journal
//! rotation, the reclamation — executes behind the EXCLUSIVE lease:
//! the phase flips first (no new optional admissions, ticks, or
//! scheduling while the compound is active), a FIFO BARRIER is
//! enqueued behind every previously admitted canonical job and
//! waited out, every preceding completion is harvested into the
//! ledger and every uncertain disposition RECONCILED against the
//! canonical chain (Installed / NotInstalled / Unverifiable — never
//! a guessed confirmation), the worker and journal authority are
//! verified, and only then does the compound run with no optional
//! interleaving; the lease releases on every exit with the
//! crossing's own coherent failure semantics. THE GRACEFUL DURABLE
//! CLOSE: the same drain, then the final snapshot published as a
//! REQUIRED operation (the blocking admission, never a droppable
//! optional), its installation verified by readback, the provider
//! lifecycle checkpoint AFTER the verified durability, and the
//! honest [`CloseReport`] — never a "safely saved" claim on a bare
//! enqueue or a clean thread join.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvError, TryRecvError};

use engine_core::{EventId, EventSpec, LodLevel, PresenceSample, ScheduleError, Spine, Tick};
use engine_persist::checkpoint::peek_object_domain;
use engine_persist::{
    AppendOutcome, CanonicalMedium, CommitOutcome, Hash, JournalRotation, KeyPhase, MediumError,
    PipelineError, ReconcileOutcome, Reconciliation, RevisionRecord, StoredObject, SyncTier,
    T1Journal, TransactionId, UnknownReason, active_journal_path,
};

use crate::crossing::{
    CheckpointReport, ReclamationReport, SESSION_SNAPSHOT_DOMAIN, SessionCrossing, TransitionError,
    is_session_object, save_session,
};
use crate::npc::{NPC_COUNT, NpcWorld, REGION_COUNT};
use crate::region::RegionDirector;
use crate::store::{MediumSource, StoreLocation};
use crate::worker::{IoWorker, MediumOp, TrySubmitError, WorkerReply, WorkerSource};

// -------------------------------------------------------- the coverage model

/// The structured consequential-state coverage a snapshot capture
/// carries (the accepted P4-5A model — NEVER an opaque content hash):
/// the per-NPC materialized coverage (the ledger's applied amounts
/// composed with the lane summary's carried totals), the summarized
/// lane boundaries per region, and the outstanding obligation
/// identities. Derived from the same authoritative state the
/// serialized bytes represent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageFrontier {
    /// Per-NPC materialized coverage: the applied lane's amounts
    /// composed with the carried aggregates.
    pub materialized: Vec<u64>,
    /// The summarized lane boundary per region (zeros when
    /// uncompacted) — monotonic by the compaction discipline.
    pub lane_boundary: [u64; REGION_COUNT],
    /// The outstanding obligation identities (the pending lane —
    /// bounded by live work; resolution shrinks it legitimately).
    pub pending: BTreeSet<engine_persist::ConsequenceId>,
}

// ------------------------------------------------------------- the capture

/// One immutable snapshot capture (the capture/submit split): the
/// serialized session bytes as a content object, the observed
/// canonical parent (a fresh read through the single worker), the
/// simulation tick, the structured coverage and the journal frontier
/// captured from the SAME authoritative state the bytes represent,
/// and the capture-validity generation (the journal flip count at
/// capture — a flip invalidates the capture permanently).
///
/// Constructed only by [`OrchestratedSession::capture_snapshot`];
/// consumed only by [`OrchestratedSession::submit_publication`].
pub struct SnapshotCapture {
    pub(crate) object: StoredObject,
    /// The snapshot content identity (the object's hash).
    pub(crate) content: Hash,
    /// The observed canonical parent at capture.
    pub(crate) parent: Hash,
    /// The observed parent's epoch (the publication record's input).
    pub(crate) parent_epoch: u64,
    /// The simulation tick at capture.
    pub(crate) tick: u64,
    /// The structured consequential coverage.
    pub(crate) coverage: CoverageFrontier,
    /// The journal frontier: the committed-transaction identity set
    /// (identity, never ordering).
    pub(crate) journal_frontier: BTreeSet<TransactionId>,
    /// The capture-validity generation (the journal flip count).
    pub(crate) generation: u64,
}

impl SnapshotCapture {
    /// The snapshot content identity.
    pub fn content_identity(&self) -> Hash {
        self.content
    }

    /// The observed canonical parent at capture.
    pub fn captured_parent(&self) -> Hash {
        self.parent
    }

    /// The simulation tick at capture.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The structured consequential coverage.
    pub fn coverage(&self) -> &CoverageFrontier {
        &self.coverage
    }

    /// The journal frontier (the committed-transaction identity set).
    pub fn journal_frontier(&self) -> &BTreeSet<TransactionId> {
        &self.journal_frontier
    }

    /// The capture-validity generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

// -------------------------------------------------------- the publication API

/// The typed nonblocking-admission refusal for an optional
/// publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitRefusal {
    /// The bounded queue is at capacity (or a REQUIRED operation is
    /// waiting — the fairness gate): nothing was admitted, no append;
    /// retry or regenerate at the next boundary.
    Busy,
    /// The worker is unavailable (poisoned or exited): nothing was
    /// admitted.
    WorkerUnavailable,
    /// The capture's generation no longer matches the crossing's — a
    /// journal-pointer flip invalidated it; a fresh capture is
    /// required.
    Stale,
    /// The session is under the maintenance lease or Closing
    /// (PP-B4 checkpoint 4): optional admissions refuse until the
    /// phase returns Active — nothing was admitted, no append.
    Suspended,
}

/// One publication completion's outcome (the honest dispositions —
/// never a "safely saved" report merely because a job was queued).
#[derive(Debug)]
pub enum PublicationOutcome {
    /// The publication append was ACCEPTED by the provider (the
    /// revision installed at the captured parent; the tier is the
    /// provider's own durability label).
    Accepted { revision: Hash, tier: SyncTier },
    /// The captured parent no longer equals the canonical HEAD — the
    /// stale capture was REJECTED by the CAS, never rebased; a fresh
    /// capture is required.
    Stale {
        revision: Hash,
        observed: Option<Hash>,
    },
    /// The disposition could not be verified (the worker died or the
    /// reply was lost around a possibly executed append) — reconcile;
    /// never interpreted as accepted or rejected.
    Unknown {
        revision: Hash,
        reason: UnknownReason,
    },
    /// A definite medium error (nothing installed).
    Failed { revision: Hash, error: MediumError },
    /// Canceled before the job started (a definite non-event).
    Canceled { revision: Hash },
}

/// One completion's retained disposition (C3-F3): the typed outcome
/// classification the in-session completion ledger retains AFTER the
/// caller consumed the one-shot poll/wait result — the authority-side
/// reconciliation (and Checkpoint 4's maintenance drain) reads THESE
/// records, never the caller's transient return value. The candidate
/// identity lives on the record itself; the class-specific evidence
/// (the provider-confirmed revision and its ACTUAL durability tier,
/// the observed stale parent, the unknown-class reason, the
/// definite-failure detail) lives here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionDisposition {
    /// Accepted by the provider at the captured parent — the
    /// provider-confirmed revision identity and its actual
    /// durability tier.
    Accepted { revision: Hash, tier: SyncTier },
    /// The stale CAS rejection (the captured parent no longer the
    /// canonical HEAD; the bytes were never rebased) — the observed
    /// parent when the provider reported one.
    Stale { observed: Option<Hash> },
    /// The unresolved class — a possibly executed append whose reply
    /// was lost (or a worker death in flight): never interpreted as
    /// accepted or rejected; the unresolved candidate identity is
    /// the record's `revision` field, retained for reconciliation.
    Unknown { reason: UnknownReason },
    /// The maintenance drain RECONCILED a previously unknown
    /// disposition against the canonical chain (PP-B4 checkpoint 4):
    /// `installed` is the VERIFIED result — never a guessed
    /// confirmation; an unverifiable state stays `Unknown`.
    Reconciled { revision: Hash, installed: bool },
    /// A definite medium failure — nothing was installed (the
    /// pre-commit class, or a job refused at the worker's execution
    /// boundary); the detail is the error's display and NEVER an
    /// implication that an append committed.
    Failed { detail: String },
    /// Canceled before the job started — a definite non-event.
    Canceled,
}

impl CompletionDisposition {
    /// The ledger's classification of one one-shot outcome.
    fn of(outcome: &PublicationOutcome) -> Self {
        match outcome {
            PublicationOutcome::Accepted { revision, tier } => CompletionDisposition::Accepted {
                revision: *revision,
                tier: *tier,
            },
            PublicationOutcome::Stale { observed, .. } => CompletionDisposition::Stale {
                observed: *observed,
            },
            PublicationOutcome::Unknown { reason, .. } => {
                CompletionDisposition::Unknown { reason: *reason }
            }
            PublicationOutcome::Failed { error, .. } => CompletionDisposition::Failed {
                detail: error.to_string(),
            },
            PublicationOutcome::Canceled { .. } => CompletionDisposition::Canceled,
        }
    }
}

/// The honest outcome for one publication reply — the shared mapping
/// (the poll/wait completions and the join harvest both derive it).
fn publication_outcome(
    revision: Hash,
    reply: Result<WorkerReply, MediumError>,
) -> PublicationOutcome {
    match reply {
        Ok(WorkerReply::Append(AppendOutcome::Accepted { id, tier })) => {
            PublicationOutcome::Accepted { revision: id, tier }
        }
        Ok(WorkerReply::Append(AppendOutcome::RejectedStaleParent { observed })) => {
            PublicationOutcome::Stale { revision, observed }
        }
        Ok(WorkerReply::Append(AppendOutcome::Unknown { reason })) => {
            PublicationOutcome::Unknown { revision, reason }
        }
        Ok(WorkerReply::Canceled) => PublicationOutcome::Canceled { revision },
        Ok(other) => unreachable!("the publication reply was {other:?}"),
        Err(error) => PublicationOutcome::Failed { revision, error },
    }
}

/// The maintenance/close gate's short detail (the established
/// detail strings pass through; anything else carries its debug
/// form).
fn gate_detail(error: &TransitionError) -> String {
    match error {
        TransitionError::InstallIncompatible { detail, .. } => (*detail).to_owned(),
        other => format!("{other:?}"),
    }
}

/// One retained completion record: the candidate identity, the typed
/// disposition, and the captured frontier survive the completion —
/// the accounting never depends on the caller's polling, and an
/// outcome the caller already consumed remains available for the
/// authority-side reconciliation (C3-F3).
#[derive(Debug, Clone)]
pub struct CompletionRecord {
    /// The publication's orchestrator-local id.
    pub id: u64,
    /// The candidate's identity (the publication record's id) —
    /// retained for every class, including the unresolved Unknown.
    pub revision: Hash,
    /// The retained typed disposition.
    pub disposition: CompletionDisposition,
    /// The captured consequential coverage.
    pub coverage: CoverageFrontier,
    /// The captured journal frontier.
    pub journal_frontier: BTreeSet<TransactionId>,
}

/// A submitted publication's handle (poll, wait, or cancel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicationHandle {
    /// The orchestrator-local publication id.
    pub id: u64,
}

/// The scheduling operation's typed error: the composed gate refusal
/// or the accepted spine scheduling error.
#[derive(Debug)]
pub enum ScheduleOpError {
    /// The composed authority gate refused (the journal authority or
    /// the worker health) — no scheduling happened.
    Gate(TransitionError),
    /// The spine's own scheduling refusal (the accepted dependency
    /// validation).
    Schedule(ScheduleError),
}

/// One authoritative tick's report.
#[derive(Debug, Clone)]
pub struct TickReport {
    /// The executed simulation tick.
    pub tick: Tick,
    /// The regions that transitioned this tick (in execution order).
    pub regions_transitioned: Vec<usize>,
}

/// The join report (C3-F1): the worker thread's disposition and the
/// FINAL in-session completion ledger — every previously polled
/// completion plus the harvested dispositions of the unpolled
/// pending publications. NEVER a "safely saved" claim: a clean join
/// says only that the thread exited and the accounting is complete.
#[derive(Debug)]
pub struct JoinReport {
    /// The worker thread's join result (Ok on the clean drain; a
    /// panicked worker surfaces as the typed unavailable error).
    pub worker: Result<(), MediumError>,
    /// The final completion ledger: the candidate identity, the
    /// typed disposition, and the captured frontier for every
    /// publication this session admitted.
    pub completions: Vec<CompletionRecord>,
}

/// The orchestrated session's phase (PP-B4 checkpoint 4): `Active`
/// admits everything; `Maintenance` — a protected maintenance
/// compound is executing (or its barrier waiting); `Closing` — the
/// graceful durable close is in progress. Authoritative ticks,
/// scheduling, and optional admissions refuse outside `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// The ordinary operational phase.
    Active,
    /// A protected maintenance compound is active (the exclusive
    /// lease — the maintenance operations).
    Maintenance,
    /// The graceful durable close is in progress (terminal).
    Closing,
}

/// The protected maintenance failure surface (PP-B4 checkpoint 4):
/// the GATE failures (the lease never established — the worker
/// unavailable, the journal authority lost or unresolved, an
/// unverifiable canonical disposition) are distinct from the
/// COMPOUND failures (the maintenance operation itself refused after
/// the drain — the crossing's own coherent semantics, each effect
/// individually durable or nothing ran).
#[derive(Debug)]
pub enum MaintenanceError {
    /// The maintenance gate refused: the worker unavailable, the
    /// journal authority lost or unresolved, or an uncertain
    /// canonical disposition unverifiable — the compound NEVER began;
    /// the session state is unchanged (the phase released).
    Gate(TransitionError),
    /// The compound itself failed AFTER the drain — the crossing's
    /// own typed failure with its coherent state semantics; the
    /// lease released, the retry coherent.
    Compound(TransitionError),
}

/// The graceful close's failure classes (PP-B4 checkpoint 4): an
/// honest FAILED close — never a claimed save on a bare enqueue or a
/// clean thread join.
#[derive(Debug)]
pub enum CloseFailure {
    /// The worker became unavailable during the close (dead,
    /// poisoned, or refused at the barrier's execution boundary): no
    /// final snapshot was published.
    WorkerUnavailable,
    /// An uncertain canonical disposition could not be verified
    /// against the chain — never a guessed confirmation.
    UnverifiableReconciliation,
    /// The journal-authority gate refused the close (the R4/R5
    /// states); the detail names the state.
    Authority(String),
    /// The final publication failed definitely (the medium's own
    /// error, as its display).
    PublicationFailed(String),
    /// The final publication's readback verification failed (the
    /// installed record or its content object did not verify against
    /// the capture).
    ReadbackFailed(String),
    /// The provider lifecycle checkpoint failed (as its display).
    CheckpointFailed(String),
    /// The bounded recapture discipline was exhausted (the captured
    /// parent kept going stale — the bytes were never rebased).
    RecaptureExhausted,
    /// A T1-durable required consequential confirmation remains
    /// UNSETTLED after the bounded retry discipline (C4-R1): the
    /// obligation is explicitly left for the crash-recovery
    /// re-drive — the T1 evidence is preserved and a fresh recovery
    /// re-drives it exactly-once — but the close is NEVER a fully
    /// successful one; any verified snapshot evidence stands in
    /// `final_revision` and `required_settled` is false.
    UnsettledRequired,
    /// A NEWER DIFFERENT session snapshot is the authoritative
    /// recovery anchor (C4-R2): the current state cannot be made
    /// authoritative without violating the chain-wide idempotency
    /// rule (its content hash is already committed below the
    /// superseding snapshot). The close fails closed — never a
    /// success claimed from the superseded older revision.
    SupersededSnapshot,
    /// The worker thread did not join cleanly (a panic during the
    /// drain/teardown — the join detail is carried in `worker`):
    /// never a fully successful close.
    WorkerJoinFailed,
}

/// The graceful close's honest report (PP-B4 checkpoint 4): the
/// final required snapshot's VERIFIED revision and tier (the
/// legitimate verification EVIDENCE — retained even when a later
/// close step failed, since the snapshot itself stands verified on
/// the chain; None only when no final snapshot was verified), the
/// lifecycle-checkpoint and required-confirmation outcomes, the
/// final completion ledger, the worker thread's join result, and
/// the overall failure state. THE OVERALL OUTCOME: `failure == None`
/// is the COMPLETE successful close — the verified final snapshot +
/// the completed provider lifecycle checkpoint + the settled
/// required consequential confirmations + the clean join; a failure
/// never erases the verification evidence already earned. A clean
/// thread join is NEVER a successful durable close.
#[derive(Debug)]
pub struct CloseReport {
    /// The final required snapshot's readback-verified revision —
    /// the verification EVIDENCE (retained even when a later close
    /// step failed: a `CheckpointFailed` close still carries its
    /// verified snapshot). None only when no final snapshot was
    /// verified — never a claimed save.
    pub final_revision: Option<Hash>,
    /// The final publication's provider durability tier.
    pub final_tier: Option<SyncTier>,
    /// Whether the provider lifecycle checkpoint completed (the
    /// ratified SQLite WAL TRUNCATE; the file medium's no-op).
    pub lifecycle_checkpoint: bool,
    /// Whether every required consequential confirmation settled
    /// canonically before the teardown. False means an unconfirmed
    /// T1-durable obligation was EXPLICITLY left for the
    /// crash-recovery re-drive — reported, never a claim that all
    /// consequential progression is canonically durably saved.
    pub required_settled: bool,
    /// The final completion ledger: every publication this session
    /// admitted, with its typed disposition (the harvested uncertain
    /// dispositions included — never silently discarded).
    pub completions: Vec<CompletionRecord>,
    /// The worker thread's join result.
    pub worker: Result<(), MediumError>,
    /// The overall close outcome — None ONLY on the complete
    /// successful close (the verified snapshot + the lifecycle
    /// checkpoint + the settled confirmations + the clean join).
    pub failure: Option<CloseFailure>,
}

// ------------------------------------------------------------- the session

/// The orchestrated authoritative session (PP-B4 checkpoint 3): the
/// narrow owner of the simulation state, the worker-backed crossing,
/// and the worker — the ONLY path to authoritative progression. No
/// unrestricted mutable access to the `Spine` or `RegionDirector`
/// escapes this type; scheduling is the separate
/// [`Self::schedule_event`] operation; the optional publications go
/// through the single worker's bounded queue.
pub struct OrchestratedSession {
    spine: Spine<NpcWorld>,
    director: RegionDirector,
    crossing: SessionCrossing<WorkerSource>,
    worker: IoWorker,
    /// The campaign journal ROOT (the pointer's authoritative
    /// location — never the resolved active generation).
    journal_root: PathBuf,
    /// The fairness gate: set while a REQUIRED canonical operation is
    /// waiting — optional admissions refuse so continued optional
    /// submissions can never indefinitely starve the required
    /// operation under finite worker service.
    required_waiting: AtomicBool,
    /// The session's phase (PP-B4 checkpoint 4): `Active` admits
    /// everything; `Maintenance` — a protected compound is executing
    /// (or its barrier waiting); `Closing` — the graceful close.
    /// The phase gates the ticks, the scheduling, and the optional
    /// admissions.
    phase: SessionPhase,
    /// The pending optional publications (the completion slots the
    /// orchestrator RETAINS — caller cancellation never orphans the
    /// accounting). IN-MEMORY GROWTH HONESTY (C3-F3): the bounded
    /// queue bounds ADMISSION, never the lifetime accumulation of
    /// pending and completed records — an unpolled completion stays
    /// pending with its live reply slot until consumed (never
    /// silently discarded to enforce an artificial bound); Checkpoint
    /// 4's maintenance drain harvests exactly these retained slots
    /// (the same try_recv harvest [`Self::join`] performs).
    pending: Vec<PendingPublication>,
    /// The retained completion records — every typed disposition
    /// survives the caller's one-shot consumption (the authority-side
    /// reconciliation reads these). The same in-memory growth honesty
    /// applies: unbounded over the session's lifetime by design.
    completed: Vec<CompletionRecord>,
    next_id: u64,
}

struct PendingPublication {
    id: u64,
    /// The candidate's identity and the captured frontier — retained
    /// until (and through) the completion.
    revision: Hash,
    coverage: CoverageFrontier,
    journal_frontier: BTreeSet<TransactionId>,
    slot: Receiver<Result<WorkerReply, MediumError>>,
    /// The cancellation token — effective only before the job starts.
    cancel: Arc<AtomicBool>,
}

impl OrchestratedSession {
    /// Opens the orchestrated session over an ALREADY-FOUNDED store
    /// location (the pre-worker discipline: the caller founds — or
    /// recovers — first), taking ownership of the simulation state.
    /// The journal must be the campaign ROOT journal (a fresh
    /// session's own journal — no rotation has happened yet).
    pub fn open(
        spine: Spine<NpcWorld>,
        director: RegionDirector,
        location: &StoreLocation,
        journal: T1Journal,
        campaign: &str,
    ) -> Result<Self, TransitionError> {
        let journal_root = journal.path().to_path_buf();
        let worker = IoWorker::spawn(location).map_err(PipelineError::Medium)?;
        let source = worker.source().map_err(PipelineError::Medium)?;
        let crossing = SessionCrossing::open_at(&source, journal, campaign)?;
        Ok(Self {
            spine,
            director,
            crossing,
            worker,
            journal_root,
            required_waiting: AtomicBool::new(false),
            phase: SessionPhase::Active,
            pending: Vec::new(),
            completed: Vec::new(),
            next_id: 0,
        })
    }

    /// Recovers the orchestrated session from the durable state alone
    /// (the pre-worker discipline: the recovery runs over the LEGACY
    /// source, single-threaded, before the worker exists; the
    /// recovered crossing is dropped — releasing the journal's
    /// sidecar lock — and a fresh worker-backed crossing re-derives
    /// from the pointer-resolved ACTIVE journal, the campaign ROOT
    /// preserved).
    pub fn recover(
        location: &StoreLocation,
        journal_path: &Path,
        campaign: &str,
    ) -> Result<Self, TransitionError> {
        let (spine, director, legacy) =
            SessionCrossing::recover_at(location, journal_path, campaign)?;
        drop(legacy);
        let active = active_journal_path(journal_path).map_err(PipelineError::Journal)?;
        let journal = T1Journal::open(&active).map_err(PipelineError::Journal)?;
        let worker = IoWorker::spawn(location).map_err(PipelineError::Medium)?;
        let source = worker.source().map_err(PipelineError::Medium)?;
        let mut crossing = SessionCrossing::open_at(&source, journal, campaign)?;
        crossing.set_journal_root(journal_path.to_path_buf());
        Ok(Self {
            spine,
            director,
            crossing,
            worker,
            journal_root: journal_path.to_path_buf(),
            required_waiting: AtomicBool::new(false),
            phase: SessionPhase::Active,
            pending: Vec::new(),
            completed: Vec::new(),
            next_id: 0,
        })
    }

    /// THE authoritative tick (the Revision-6 driver): the composed
    /// gate first (the journal authority AND the worker health — a
    /// dead or poisoned worker refuses BEFORE any progression,
    /// unpolled completions notwithstanding); the presence-driven
    /// target derivation; the exclusive routing; `run_tick` ONLY
    /// after every required durable transition succeeded. A failure
    /// aborts BEFORE the clock advances — the transitions already
    /// completed stand (each individually durable), and the retry's
    /// level check skips them. NO scheduling inside the tick.
    pub fn tick(&mut self, presence: &[PresenceSample]) -> Result<TickReport, TransitionError> {
        self.assert_active()?;
        self.crossing.assert_authoritative()?;
        let now = self.spine.clock();
        let mut regions_transitioned = Vec::new();
        for region in 0..REGION_COUNT {
            let current = self.director.levels()[region];
            let next = self.director.target_level(region, presence);
            if next == current {
                continue;
            }
            if next == LodLevel::Dormant {
                // Live→dormant: the SIX-PHASE durable crossing (the T1
                // journal transaction and its fsync precede the
                // destructive simulation changes).
                self.crossing
                    .deactivate(&mut self.spine, &mut self.director, region, now)?;
                regions_transitioned.push(region);
            } else if current == LodLevel::Dormant {
                // Dormant→live: the durable reactivation (the live
                // application of the already-durable captures).
                self.crossing
                    .activate(&mut self.spine, &mut self.director, region, next, now)?;
                // C3-F4: the crossing preserves the LEGACY
                // Ok-on-coherent-refusal semantics — a refused
                // activation leaves the region safely dormant. The
                // orchestrated tick REFUSES on it: a typed error
                // BEFORE `run_tick` (the clock never advances, the
                // incomplete transition is never recorded as
                // successful); the transitions already completed this
                // tick stand, and the next boundary retries coherently
                // (the level check skips what completed).
                if self.director.levels()[region] != next {
                    return Err(TransitionError::ActivationRefused { region });
                }
                regions_transitioned.push(region);
            } else {
                // Ordinary live→live: the accepted safe transition path.
                self.director
                    .transition_region(&mut self.spine, region, next, now);
                regions_transitioned.push(region);
            }
        }
        let tick = self.spine.run_tick();
        Ok(TickReport {
            tick,
            regions_transitioned,
        })
    }

    /// The SEPARATE scheduling operation (the accepted option-B
    /// design): the composed gate first (a dead or poisoned worker
    /// refuses scheduling too), then the accepted region-aware
    /// dispatch VERBATIM — the spine's own dependency validation,
    /// identity allocation, and the dormant-hold.
    pub fn schedule_event(&mut self, spec: EventSpec) -> Result<EventId, ScheduleOpError> {
        self.assert_active().map_err(ScheduleOpError::Gate)?;
        self.crossing
            .assert_authoritative()
            .map_err(ScheduleOpError::Gate)?;
        self.director
            .schedule(&mut self.spine, spec)
            .map_err(ScheduleOpError::Schedule)
    }

    /// The REQUIRED canonical confirmation (enqueue-and-wait through
    /// the single worker): the fairness gate is armed for the
    /// duration — optional admissions refuse while this waits, so
    /// continued optional submissions can never indefinitely starve
    /// the required operation under finite worker service. Optional
    /// backpressure never touches this path's T1 durability (the
    /// journal writes precede the confirmation on the authority
    /// thread).
    pub fn commit_canonical(&mut self) -> Result<CommitOutcome, TransitionError> {
        self.assert_active()?;
        self.crossing.assert_authoritative()?;
        self.required_waiting.store(true, Ordering::Relaxed);
        let result = self.crossing.commit_canonical();
        self.required_waiting.store(false, Ordering::Relaxed);
        result
    }

    /// Captures one immutable snapshot (read-only — no queue writes,
    /// no journal appends): the serialized session bytes, the observed
    /// canonical parent (a fresh read through the single worker), the
    /// structured coverage and journal frontier from the SAME
    /// authoritative state the bytes represent, and the
    /// capture-validity generation.
    pub fn capture_snapshot(&self) -> Result<SnapshotCapture, TransitionError> {
        let bytes = save_session(&self.spine, &self.director).map_err(PipelineError::Format)?;
        let object = StoredObject::new(bytes).map_err(PipelineError::Medium)?;
        let coverage = self.coverage_frontier();
        // C3-F2: the frontier derives from the AUTHORITATIVE ACTIVE
        // journal generation — the pointer-resolved path, never the
        // root file itself (after a PP-B3 rotation the root still
        // holds the retired transactions and lacks the post-rotation
        // ones). The ROOT stays the pointer's authoritative location
        // — never replaced by the resolved generation; a campaign
        // without a pointer resolves to the root itself (the
        // pre-rotation behavior, byte-identical), and a pointer
        // naming a missing journal fails the capture closed.
        let active = active_journal_path(&self.journal_root).map_err(PipelineError::Journal)?;
        let scan = T1Journal::scan_file(&active).map_err(PipelineError::Journal)?;
        let journal_frontier = scan
            .committed
            .iter()
            .map(|transaction| transaction.id)
            .collect();
        let (parent, parent_epoch) = self.observe_head()?;
        Ok(SnapshotCapture {
            content: object.hash(),
            object,
            parent,
            parent_epoch,
            tick: self.spine.clock().as_u64(),
            coverage,
            journal_frontier,
            generation: self.crossing.journal_flip_count(),
        })
    }

    /// Submits an optional snapshot publication (NONBLOCKING
    /// admission): the generation check first (a journal flip
    /// invalidated the capture → the typed stale, no append); the
    /// fairness gate (a waiting required operation → Busy); then the
    /// bounded try-admission through the single worker — a full or
    /// unavailable queue returns the typed refusal with NO canonical
    /// append. Once admitted, the publication executes FIFO against
    /// the CAPTURED parent (never a newly selected parent); a HEAD
    /// advance surfaces the stale rejection — the bytes are NEVER
    /// rebased.
    pub fn submit_publication(
        &mut self,
        capture: SnapshotCapture,
    ) -> Result<PublicationHandle, SubmitRefusal> {
        if self.phase != SessionPhase::Active {
            return Err(SubmitRefusal::Suspended);
        }
        if capture.generation != self.crossing.journal_flip_count() {
            return Err(SubmitRefusal::Stale);
        }
        if self.required_waiting.load(Ordering::Relaxed) {
            return Err(SubmitRefusal::Busy);
        }
        let record = RevisionRecord::regular(capture.parent, capture.parent_epoch, capture.tick)
            .with_idempotency_key(capture.content.as_bytes().to_vec())
            .with_content_refs(vec![capture.content]);
        let revision = record.id();
        let cancel = Arc::new(AtomicBool::new(false));
        let slot = self
            .worker
            .try_submit(
                MediumOp::Append {
                    record,
                    objects: vec![capture.object],
                    expected_parent: Some(capture.parent),
                },
                Some(cancel.clone()),
            )
            .map_err(|refusal| match refusal {
                TrySubmitError::QueueFull => SubmitRefusal::Busy,
                TrySubmitError::Unavailable => SubmitRefusal::WorkerUnavailable,
            })?;
        let id = self.next_id;
        self.next_id += 1;
        self.pending.push(PendingPublication {
            id,
            revision,
            coverage: capture.coverage,
            journal_frontier: capture.journal_frontier,
            slot,
            cancel,
        });
        Ok(PublicationHandle { id })
    }

    /// Polls one publication's completion (nonblocking): `None` while
    /// queued or started-but-unfinished (or the id is unknown); the
    /// outcome otherwise. The accounting never depends on the
    /// caller's polling — the orchestrator retains every completion
    /// record.
    pub fn poll_publication(&mut self, handle: PublicationHandle) -> Option<PublicationOutcome> {
        let index = self
            .pending
            .iter()
            .position(|entry| entry.id == handle.id)?;
        match self.pending[index].slot.try_recv() {
            Ok(reply) => Some(self.complete_at(index, reply)),
            Err(TryRecvError::Empty) => None,
            // The worker died with this job in flight — the possibly
            // executed append's disposition is UNKNOWN, never
            // interpreted as accepted or rejected.
            Err(TryRecvError::Disconnected) => {
                let outcome = self.unknown_at(index);
                Some(outcome)
            }
        }
    }

    /// Waits for one publication's completion (blocking).
    pub fn wait_publication(&mut self, handle: PublicationHandle) -> Option<PublicationOutcome> {
        let index = self
            .pending
            .iter()
            .position(|entry| entry.id == handle.id)?;
        match self.pending[index].slot.recv() {
            Ok(reply) => Some(self.complete_at(index, reply)),
            Err(RecvError) => {
                let outcome = self.unknown_at(index);
                Some(outcome)
            }
        }
    }

    /// Cancels a queued publication — effective ONLY before the job
    /// starts (the worker checks the token once, before execution); a
    /// job already started runs to its completion boundary and its
    /// possibly-committed outcome is tracked and reported, never
    /// erased.
    pub fn cancel_publication(&mut self, handle: PublicationHandle) -> bool {
        match self.pending.iter_mut().find(|entry| entry.id == handle.id) {
            Some(entry) => {
                entry.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// The retained completion record for one publication id (the
    /// candidate identity and the captured frontier survive the
    /// completion).
    pub fn completion_record(&self, handle: PublicationHandle) -> Option<&CompletionRecord> {
        self.completed.iter().find(|record| record.id == handle.id)
    }

    /// The number of pending (admitted, unfinished) publications.
    pub fn pending_publications(&self) -> usize {
        self.pending.len()
    }

    /// The worker's health (readable without a blocking request).
    pub fn healthy(&self) -> bool {
        self.worker.healthy()
    }

    /// The session's current phase (PP-B4 checkpoint 4): `Active`
    /// admits everything; `Maintenance` — a protected compound is
    /// executing; `Closing` — the graceful close (terminal).
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    /// POISON the session (the worker's terminal lifecycle — the
    /// delegated discipline): every authoritative operation refuses
    /// through the composed gate until a fresh recovery.
    pub fn poison(&self) {
        self.worker.poison();
    }

    /// Test-only: the fairness gate's armed state (the
    /// required-waiting flag the optional admissions refuse under —
    /// exercised deterministically without a threaded interleaving).
    #[doc(hidden)]
    pub fn set_required_waiting_for_test(&self, armed: bool) {
        self.required_waiting.store(armed, Ordering::Relaxed);
    }

    /// The worker's cached durability tier (the provider's truthful
    /// label, read at spawn).
    pub fn worker_tier(&self) -> SyncTier {
        self.worker.tier()
    }

    /// The capture-validity generation (the journal flip count).
    pub fn journal_flip_count(&self) -> u64 {
        self.crossing.journal_flip_count()
    }

    /// The simulation clock (read-only).
    pub fn clock(&self) -> Tick {
        self.spine.clock()
    }

    /// One region's current LOD level (read-only).
    pub fn region_level(&self, region: usize) -> LodLevel {
        self.director.levels()[region]
    }

    /// Read-only spine access — the Slice 0 integration
    /// demonstration's narrow read-only seam: the session remains the
    /// ONLY owner and mutator of the authoritative state; derived
    /// rendering, overlays, and run reports read downstream through
    /// this reference (the same read-only surface the pipeline-free
    /// render demo consumes from its own driver). No mutable access
    /// to the spine escapes this type.
    pub fn spine(&self) -> &Spine<NpcWorld> {
        &self.spine
    }

    /// Read-only region-director access — levels, config, the capture
    /// ledger, diagnostics, and activation-latency percentiles for
    /// derived rendering and overlays (the same read-only surface the
    /// nonpersistent render demo consumes from its own driver).
    pub fn director(&self) -> &RegionDirector {
        &self.director
    }

    /// The per-key durable status passthrough
    /// (`SessionCrossing::key_phase`): the per-consequence
    /// pending-vs-confirmed display input — the truthful gating
    /// surface (a key is confirmed only `InConfirmedChain`).
    pub fn key_phase(&self, id: engine_persist::ConsequenceId) -> Option<KeyPhase> {
        self.crossing.key_phase(id)
    }

    /// Test-only: the worker's per-instance fault control.
    #[doc(hidden)]
    pub fn fault_control(&self) -> crate::worker::WorkerFaultControl {
        self.worker.fault_control()
    }

    /// Joins the worker (the bounded clean-shutdown accounting,
    /// C3-F1): the crossing — and the pipeline's queue proxies it
    /// owns — is released FIRST (the worker loop exits only when
    /// EVERY queue sender is dropped; joining while the crossing
    /// lives would block indefinitely). With the senders released,
    /// the worker drains every admitted job FIFO (admitted work is
    /// never abandoned — the C2-F2 discipline) and exits; the
    /// unpolled completion slots are then harvested into the
    /// completion ledger with their honest dispositions — the same
    /// harvest Checkpoint 4's maintenance drain will perform (a slot
    /// with no reply after the exit is the unresolved Unknown class,
    /// never interpreted, never silently discarded).
    ///
    /// This is NOT the graceful final-save shutdown (Checkpoint 4's
    /// scope): no final required snapshot is published, and a clean
    /// join is NEVER itself a claim that anything was safely saved —
    /// the dispositions in the report speak for themselves.
    pub fn join(self) -> JoinReport {
        // Release the crossing first — the sender-lifetime deadlock
        // (C3-F1): the pipeline's proxy and the crossing's source
        // hold queue senders, and the loop's exit requires every one
        // of them dropped BEFORE the join blocks.
        let Self {
            spine,
            director,
            crossing,
            worker,
            journal_root: _,
            required_waiting: _,
            phase: _,
            pending,
            completed,
            next_id: _,
        } = self;
        drop(spine);
        drop(director);
        drop(crossing);
        // The pending entries hold only reply RECEIVERS — retaining
        // them through the join is safe (an abandoned reply is a
        // typed send failure the worker ignores).
        let thread = worker.join();
        let mut completions = completed;
        for entry in pending {
            let outcome = match entry.slot.try_recv() {
                Ok(reply) => publication_outcome(entry.revision, reply),
                // No reply after the thread's exit (a death in flight
                // or a dropped reply slot): the unresolved class.
                Err(_) => PublicationOutcome::Unknown {
                    revision: entry.revision,
                    reason: UnknownReason::WorkerReplyLost,
                },
            };
            completions.push(CompletionRecord {
                id: entry.id,
                revision: entry.revision,
                disposition: CompletionDisposition::of(&outcome),
                coverage: entry.coverage,
                journal_frontier: entry.journal_frontier,
            });
        }
        JoinReport {
            worker: thread,
            completions,
        }
    }

    // ------------------------------------------------- protected maintenance

    /// The session-phase gate (PP-B4 checkpoint 4): authoritative
    /// gameplay and optional admissions refuse outside Active — a
    /// maintenance compound is executing, or the session is Closing.
    /// Nothing ran; the durable state is untouched.
    fn assert_active(&self) -> Result<(), TransitionError> {
        match self.phase {
            SessionPhase::Active => Ok(()),
            SessionPhase::Maintenance => Err(TransitionError::PhaseRefused {
                detail: "maintenance-active",
            }),
            SessionPhase::Closing => Err(TransitionError::PhaseRefused { detail: "closing" }),
        }
    }

    /// The SK/MK checkpoint under the EXCLUSIVE maintenance lease
    /// (PP-B4 checkpoint 4): the drain — the FIFO barrier, the
    /// completion harvest, the uncertain-disposition reconciliation —
    /// precedes the compound, so the checkpoint begins from the
    /// latest verified canonical state with every earlier optional
    /// publication settled, and NO optional admission can interleave
    /// between SK and MK. The crossing's own accepted checkpoint
    /// operation — the eligibility checks, the explicit spine-history
    /// compaction policy, the SK/MK adjacency and metadata
    /// commitments, the crash windows and recovery semantics —
    /// VERBATIM, over the worker-backed CanonicalMedium. An SK-only
    /// partial checkpoint is never a completed compound (the
    /// crossing's own typed failure).
    pub fn publish_checkpoint(
        &mut self,
        retain_tail: u64,
    ) -> Result<CheckpointReport, MaintenanceError> {
        self.maintenance(|session| {
            session.crossing.publish_checkpoint(
                &mut session.spine,
                &mut session.director,
                retain_tail,
            )
        })
    }

    /// The journal rotation under the EXCLUSIVE maintenance lease:
    /// every earlier admitted publication settles BEFORE the
    /// rotation (the FIFO barrier — nothing of the session's is in
    /// flight across the generation flip); the accepted
    /// verified-anchor retirement rule and the
    /// canceled-vs-confirmed-subsumed classification run VERBATIM;
    /// the pipeline reopens over the active generation through the
    /// SAME worker (no independent provider connection); and the
    /// capture-validity generation increments at the mechanism's
    /// successful pointer flip — before the R5 durability gate — so
    /// every pre-flip capture is permanently rejected, even with the
    /// canonical HEAD unchanged. The fail-closed behavior after an
    /// unresolved pointer durability or a failed pipeline rebuild,
    /// and the R5 repair semantics, are the crossing's own.
    pub fn rotate_journal(&mut self) -> Result<JournalRotation, MaintenanceError> {
        self.maintenance(|session| session.crossing.rotate_journal())
    }

    /// The reclamation under the EXCLUSIVE maintenance lease: the
    /// preceding canonical publications settled and reconciled, then
    /// the accepted audited reclamation-manifest protocol VERBATIM —
    /// verify-before-delete, the idempotent restart, the
    /// whole-manifest completion check, the authoritative journal
    /// generations — through the worker-backed medium. An optional
    /// publication's Accepted report is NEVER the reclamation-safety
    /// inference: the audit's own chain verification governs.
    pub fn reclaim_checkpoint(
        &mut self,
        generations: u64,
    ) -> Result<ReclamationReport, MaintenanceError> {
        let root = self.journal_root.clone();
        self.maintenance(move |session| session.crossing.reclaim_checkpoint(&root, generations))
    }

    /// THE R5 REPAIR, exposed for the orchestrated path:
    /// re-establish the pointer's power-loss durability after an
    /// unresolved rotation — the crossing's own state machine (a
    /// re-sync failure leaves the fail-closed state intact; a
    /// rebuild failure escalates to authority-lost; healthy only
    /// after the verified durability). Not under the maintenance
    /// envelope: the repair writes nothing canonical (the directory
    /// re-sync plus the pipeline rebuild over the pointer-resolved
    /// journal).
    pub fn reestablish_pointer_durability(&mut self) -> Result<(), TransitionError> {
        self.crossing.reestablish_pointer_durability()
    }

    /// THE MAINTENANCE ENVELOPE (the exclusive lease, the GO §2
    /// sequence): the phase flips FIRST (no new optional admissions,
    /// ticks, or scheduling while the compound is active); the gate
    /// drains — the FIFO barrier behind every previously admitted
    /// canonical job, the completion harvest into the ledger, the
    /// uncertain-disposition reconciliation against the chain, the
    /// health verification; the compound then runs with no optional
    /// interleaving; and the phase releases on EVERY exit (a failed
    /// compound leaves the crossing's own coherent state — each
    /// effect individually durable or nothing ran — and the retry is
    /// coherent). No mutex the worker needs is held while waiting:
    /// the authority thread is the only submitter and the worker
    /// never calls back.
    fn maintenance<T>(
        &mut self,
        compound: impl FnOnce(&mut Self) -> Result<T, TransitionError>,
    ) -> Result<T, MaintenanceError> {
        self.phase = SessionPhase::Maintenance;
        let result = match self.enter_maintenance() {
            Err(error) => Err(error),
            Ok(()) => compound(self).map_err(MaintenanceError::Compound),
        };
        self.phase = SessionPhase::Active;
        result
    }

    /// Test-only: the maintenance envelope's PROBE — the full gate
    /// (the barrier, the harvest, the reconciliation) runs under the
    /// REAL lease, then the probe observes the exclusivity through
    /// the real path (the phase, the drained ledger, the refused
    /// ticks, scheduling, and optional admissions) and its value is
    /// returned; the lease releases without a compound.
    #[doc(hidden)]
    pub fn debug_maintenance_probe_for_test<T>(
        &mut self,
        probe: impl FnOnce(&mut Self) -> T,
    ) -> Result<T, MaintenanceError> {
        self.phase = SessionPhase::Maintenance;
        let result = match self.enter_maintenance() {
            Err(error) => Err(error),
            Ok(()) => Ok(probe(self)),
        };
        self.phase = SessionPhase::Active;
        result
    }

    /// THE MAINTENANCE GATE: the composed authority gate first (a
    /// dead or poisoned worker, a lost or unresolved journal
    /// authority — maintenance fails CLOSED, never a queue bypass
    /// through a direct provider handle); the FIFO BARRIER enqueued
    /// behind every previously admitted canonical job and waited
    /// out; the completion harvest; the uncertain-disposition
    /// reconciliation; the post-drain health verification.
    fn enter_maintenance(&mut self) -> Result<(), MaintenanceError> {
        self.crossing
            .assert_authoritative()
            .map_err(MaintenanceError::Gate)?;
        // THE FIFO BARRIER: a REQUIRED (blocking) admission — it
        // waits for queue room while the worker drains, so it lands
        // BEHIND every previously admitted job; its reply proves they
        // all executed and replied. A poisoned worker refuses the
        // barrier at its own execution boundary (the definite
        // unavailable class) — fail closed, never a bypass.
        let slot = self
            .worker
            .submit_required(MediumOp::Barrier)
            .map_err(|_| {
                MaintenanceError::Gate(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "worker/unavailable",
                })
            })?;
        match slot.recv() {
            Ok(Ok(WorkerReply::Barrier)) => {}
            Ok(Ok(other)) => unreachable!("the barrier reply was {other:?}"),
            // A refusal at the worker's execution boundary (the
            // definite unavailable class) or a death before the
            // barrier: maintenance fails closed.
            Ok(Err(_)) | Err(_) => {
                return Err(MaintenanceError::Gate(
                    TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "worker/unavailable",
                    },
                ));
            }
        }
        // THE COMPLETION HARVEST: behind the barrier every admitted
        // publication has executed and replied — drain the pending
        // into the ledger (a missing reply is the unresolved class,
        // reconciled next).
        self.harvest_pending();
        // THE RECONCILIATION: every uncertain canonical disposition
        // resolves against the chain before the compound — never a
        // guessed confirmation, and never a compound over an
        // unresolved canonical disposition.
        self.reconcile_uncertain().map_err(MaintenanceError::Gate)?;
        // THE POST-DRAIN HEALTH VERIFICATION.
        if !self.worker.healthy() {
            return Err(MaintenanceError::Gate(
                TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "worker/unavailable",
                },
            ));
        }
        self.crossing
            .assert_authoritative()
            .map_err(MaintenanceError::Gate)?;
        Ok(())
    }

    /// THE COMPLETION HARVEST: drains every pending publication slot
    /// into the ledger. Behind the barrier every admitted job has
    /// executed and replied — a missing reply is the unresolved
    /// class (a lost reply or a death in flight), retained honestly
    /// for the reconciliation, never silently discarded.
    fn harvest_pending(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for entry in pending {
            match entry.slot.try_recv() {
                Ok(reply) => {
                    let outcome = publication_outcome(entry.revision, reply);
                    let disposition = CompletionDisposition::of(&outcome);
                    self.retire_entry(entry, disposition);
                }
                Err(_) => {
                    self.retire_entry(
                        entry,
                        CompletionDisposition::Unknown {
                            reason: UnknownReason::WorkerReplyLost,
                        },
                    );
                }
            }
        }
    }

    /// THE UNCERTAIN-DISPOSITION RECONCILIATION: the crossing's
    /// retained pending required attempt first (the pipeline's own
    /// exception-safe lifecycle), then the completion ledger's
    /// Unknown dispositions — each resolves against the canonical
    /// chain through the single worker: Installed records the
    /// VERIFIED acceptance, NotInstalled the VERIFIED absence, and
    /// Unverifiable FAILS THE GATE (the uncertainty retained — never
    /// a guessed confirmation; the compounds never run over an
    /// unresolved canonical disposition).
    fn reconcile_uncertain(&mut self) -> Result<(), TransitionError> {
        if let ReconcileOutcome::Unverifiable { .. } = self.crossing.reconcile_pending()? {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "maintenance/unverifiable-attempt",
            });
        }
        let uncertain: Vec<(u64, Hash)> = self
            .completed
            .iter()
            .filter(|record| matches!(record.disposition, CompletionDisposition::Unknown { .. }))
            .map(|record| (record.id, record.revision))
            .collect();
        if uncertain.is_empty() {
            return Ok(());
        }
        let mut medium = self
            .worker
            .source()
            .map_err(PipelineError::Medium)?
            .open_medium()
            .map_err(PipelineError::Medium)?;
        for (id, revision) in uncertain {
            let installed = match medium
                .reconcile_unknown(&revision)
                .map_err(PipelineError::Medium)?
            {
                Reconciliation::Installed { .. } => true,
                Reconciliation::NotInstalled { .. } => false,
                Reconciliation::Unverifiable { .. } => {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "maintenance/unverifiable-publication",
                    });
                }
            };
            let Some(record) = self.completed.iter_mut().find(|record| record.id == id) else {
                continue;
            };
            record.disposition = CompletionDisposition::Reconciled {
                revision,
                installed,
            };
        }
        Ok(())
    }

    // ------------------------------------------------------ the graceful close

    /// THE GRACEFUL DURABLE CLOSE (the GO §6 sequence): mark Closing
    /// (new ticks, scheduling, and optional admissions refuse); the
    /// same drain as the maintenance gate (the FIFO barrier, the
    /// completion harvest, the uncertain reconciliation — an
    /// unavailable worker or an unverifiable reconciliation is the
    /// explicit FAILED close, never a claimed save); the final
    /// snapshot captured from the NEWEST authoritative state and
    /// published as a REQUIRED operation (the blocking admission,
    /// never a droppable optional) with its installation verified by
    /// readback and the bounded recapture on a stale parent (never
    /// rebased); the provider lifecycle checkpoint ONLY after the
    /// verified durability obligations; then the teardown — the T1
    /// journal ownership and the queue senders released in safe
    /// order (the crossing first, the C3-F1 discipline), the worker
    /// drained and joined. The honest [`CloseReport`]: a clean
    /// thread join is NEVER a successful durable close.
    pub fn close(self) -> CloseReport {
        self.close_inner(None::<fn(&mut Self)>)
    }

    /// Test-only: the close with a PROBE invoked after the drain,
    /// DURING the real Closing phase — the probe observes the
    /// exclusivity through the real path (the refused ticks,
    /// scheduling, and optional admissions).
    #[doc(hidden)]
    pub fn debug_close_with_probe_for_test<P>(self, probe: P) -> CloseReport
    where
        P: FnOnce(&mut Self),
    {
        self.close_inner(Some(probe))
    }

    fn close_inner<P: FnOnce(&mut Self)>(mut self, probe: Option<P>) -> CloseReport {
        // (1)+(2) CLOSING: the phase gates refuse new authoritative
        // gameplay and optional admission for the remainder of the
        // session's life.
        self.phase = SessionPhase::Closing;
        // (3)-(6) THE DRAIN: the barrier, the harvest, the
        // reconciliation — a gate failure here (an unavailable
        // worker, an unverifiable reconciliation) is the explicit
        // FAILED close: never a claimed save; the teardown still
        // runs.
        let (required_settled, gate_failure) = match self.enter_close_gate() {
            Ok(settled) => (settled, None),
            Err(failure) => (false, Some(failure)),
        };
        let mut failure = gate_failure;
        // The probe: DURING the real Closing phase, after the drain.
        if failure.is_none()
            && let Some(probe) = probe
        {
            probe(&mut self);
        }
        // (7)-(10) THE FINAL REQUIRED SNAPSHOT — only on a clean gate.
        let mut final_revision: Option<Hash> = None;
        let mut final_tier: Option<SyncTier> = None;
        if failure.is_none() {
            match self.publish_final_snapshot() {
                Ok((revision, tier)) => {
                    final_revision = Some(revision);
                    final_tier = Some(tier);
                }
                Err(close_failure) => failure = Some(close_failure),
            }
        }
        // (11)+(12) THE PROVIDER LIFECYCLE CHECKPOINT — ONLY after
        // the required durability obligations verified (the ratified
        // WAL TRUNCATE for SQLite, through the WorkerMedium; the
        // no-op for the file medium).
        let mut lifecycle_checkpoint = false;
        if failure.is_none() {
            match self.crossing.checkpoint() {
                Ok(()) => lifecycle_checkpoint = true,
                Err(error) => failure = Some(CloseFailure::CheckpointFailed(error.to_string())),
            }
        }
        // THE OVERALL-SUCCESS CONTRACT (C4-R1): `failure == None`
        // means EVERY documented success condition holds — an
        // unresolved required confirmation is an explicit INCOMPLETE
        // close (the evidence fields stand; the obligation remains
        // T1-durable for the crash-recovery re-drive).
        if failure.is_none() && !required_settled {
            failure = Some(CloseFailure::UnsettledRequired);
        }
        // (13)+(14) THE TEARDOWN: release the T1 journal ownership
        // and the remaining queue senders in safe order (the
        // crossing — and its pipeline's proxies — FIRST, the C3-F1
        // discipline), then drain and join the worker.
        let Self {
            spine,
            director,
            crossing,
            worker,
            journal_root: _,
            required_waiting: _,
            phase: _,
            pending,
            completed,
            next_id: _,
        } = self;
        drop(spine);
        drop(director);
        drop(crossing);
        let thread = worker.join();
        // A non-OK join can never coexist with a fully successful
        // overall close (C4-R1) — the join detail itself is carried
        // in `worker`.
        if thread.is_err() && failure.is_none() {
            failure = Some(CloseFailure::WorkerJoinFailed);
        }
        // The final ledger: the drain harvested the pending on a
        // clean gate; a FAILED gate may leave slots pending — they
        // are harvested here as the honest unresolved class, never
        // silently discarded.
        let mut completions = completed;
        for entry in pending {
            let disposition = match entry.slot.try_recv() {
                Ok(reply) => CompletionDisposition::of(&publication_outcome(entry.revision, reply)),
                Err(_) => CompletionDisposition::Unknown {
                    reason: UnknownReason::WorkerReplyLost,
                },
            };
            completions.push(CompletionRecord {
                id: entry.id,
                revision: entry.revision,
                disposition,
                coverage: entry.coverage,
                journal_frontier: entry.journal_frontier,
            });
        }
        // (15) THE HONEST REPORT — the four distinctions explicit:
        // the verified final snapshot, the lifecycle checkpoint, the
        // settled required confirmations, the worker join — and the
        // overall outcome (failure None ONLY on the complete
        // successful close).
        CloseReport {
            final_revision,
            final_tier,
            lifecycle_checkpoint,
            required_settled,
            completions,
            worker: thread,
            failure,
        }
    }

    /// The close's gate (the §6 steps 3-6): the same drain as the
    /// maintenance gate — plus the required-attempt settlement
    /// (C4-F3) — with the failures mapped to the explicit
    /// close-failure classes; the Ok payload is whether every
    /// required consequential confirmation settled canonically.
    fn enter_close_gate(&mut self) -> Result<bool, CloseFailure> {
        self.crossing
            .assert_authoritative()
            .map_err(|error| self.close_gate_failure(error))?;
        let slot = self
            .worker
            .submit_required(MediumOp::Barrier)
            .map_err(|_| CloseFailure::WorkerUnavailable)?;
        match slot.recv() {
            Ok(Ok(WorkerReply::Barrier)) => {}
            Ok(Ok(other)) => unreachable!("the barrier reply was {other:?}"),
            Ok(Err(_)) | Err(_) => return Err(CloseFailure::WorkerUnavailable),
        }
        self.harvest_pending();
        if let Err(error) = self.reconcile_uncertain() {
            return Err(self.close_gate_failure(error));
        }
        // THE REQUIRED-ATTEMPT SETTLEMENT (C4-F3): a T1-durable but
        // canonically unconfirmed consequential batch must not
        // silently persist through a successful close — retry it
        // through the accepted exception-safe path (no duplicate
        // consequential identity); whatever remains verified-absent
        // is EXPLICITLY reported as left for the crash-recovery
        // re-drive — never a claim that all consequential
        // progression is canonically durably saved.
        let required_settled = self
            .settle_required_attempt()
            .map_err(|error| self.close_gate_failure(error))?;
        if !self.worker.healthy() {
            return Err(CloseFailure::WorkerUnavailable);
        }
        self.crossing
            .assert_authoritative()
            .map_err(|error| self.close_gate_failure(error))?;
        Ok(required_settled)
    }

    /// THE REQUIRED-ATTEMPT SETTLEMENT (C4-F3 + C4-R3): the close's
    /// honest settlement procedure, in two phases. PHASE 1 — the
    /// retained pending canonical attempt: a verified NotInstalled
    /// attempt is RETRIED through the accepted exception-safe path
    /// (the re-walk determines keys another writer may have
    /// confirmed — never re-submitted; the re-submit carries the
    /// same identities — no duplicate consequential identity); an
    /// uncertain retry reconciles once more (bounded); whatever
    /// remains verified-absent after the bounded discipline is
    /// EXPLICITLY left for the crash-recovery re-drive. Its
    /// resolution alone is NOT settlement proof (C4-R3):
    /// `reconcile_pending`'s `NothingPending` speaks only about a
    /// RETAINED append candidate. PHASE 2 — the never-submitted
    /// actionable consequences: a journaled T1 consequence with no
    /// attempt ever created (`commit_canonical` never called) is
    /// invisible to phase 1, so the accepted canonical commit
    /// pipeline itself decides — its actionable-keys collection
    /// submits and verifies every remaining obligation under the
    /// already-established Closing gate, with the same bounded
    /// discipline governing the uncertain outcome. `Ok(true)`
    /// therefore means NO actionable consequential confirmation
    /// remains outstanding; whatever remains verified-absent (or
    /// stale-rejected) is reported as unsettled — never silently
    /// dropped, never a claim that all consequential progression is
    /// canonically durably saved. The live-session policy
    /// (authorized gameplay during pending confirmations) is
    /// untouched: this concerns only the claims at the final close
    /// boundary.
    fn settle_required_attempt(&mut self) -> Result<bool, TransitionError> {
        // PHASE 1 — the retained uncertain attempt.
        match self.crossing.reconcile_pending()? {
            ReconcileOutcome::Confirmed(_) | ReconcileOutcome::NothingPending => {}
            ReconcileOutcome::NotInstalled => {
                // THE RETRY (the accepted exception-safe path).
                match self.crossing.retry_pending()? {
                    CommitOutcome::Confirmed(_) | CommitOutcome::NothingPending => {}
                    CommitOutcome::Unknown { .. } => {
                        // ONE bounded reconciliation round.
                        match self.crossing.reconcile_pending()? {
                            ReconcileOutcome::Confirmed(_) | ReconcileOutcome::NothingPending => {}
                            // The retry's append genuinely did not
                            // land: the T1-durable obligation is
                            // EXPLICITLY left for the crash-recovery
                            // re-drive.
                            ReconcileOutcome::NotInstalled => return Ok(false),
                            ReconcileOutcome::Unverifiable { .. } => {
                                return Err(TransitionError::InstallIncompatible {
                                    region: 0,
                                    detail: "maintenance/unverifiable-attempt",
                                });
                            }
                        }
                    }
                    // The rebase-and-retry was rejected stale again —
                    // the attempt is retained for a future retry; the
                    // bounded close discipline leaves it EXPLICITLY
                    // for the crash-recovery re-drive.
                    CommitOutcome::StaleParent { .. } => return Ok(false),
                }
            }
            ReconcileOutcome::Unverifiable { .. } => {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "maintenance/unverifiable-attempt",
                });
            }
        }
        // PHASE 2 — THE NEVER-SUBMITTED ACTIONABLE CONSEQUENCES
        // (C4-R3): the accepted canonical commit pipeline under the
        // Closing gate — its actionable-keys collection decides
        // whether any journaled obligation remains unconfirmed
        // (submitting and verifying the remainder through the single
        // worker with the exact per-key provenance), and the same
        // bounded discipline as phase 1 governs the uncertain
        // outcome: one reconciliation round, one retry, then the
        // honest unsettled report.
        match self.crossing.commit_canonical()? {
            CommitOutcome::Confirmed(_) | CommitOutcome::NothingPending => Ok(true),
            CommitOutcome::Unknown { .. } => {
                // ONE bounded reconciliation round.
                match self.crossing.reconcile_pending()? {
                    ReconcileOutcome::Confirmed(_) | ReconcileOutcome::NothingPending => Ok(true),
                    ReconcileOutcome::NotInstalled => {
                        // ONE bounded retry (the accepted
                        // exception-safe path).
                        match self.crossing.retry_pending()? {
                            CommitOutcome::Confirmed(_) | CommitOutcome::NothingPending => Ok(true),
                            CommitOutcome::Unknown { .. } => {
                                match self.crossing.reconcile_pending()? {
                                    ReconcileOutcome::Confirmed(_)
                                    | ReconcileOutcome::NothingPending => Ok(true),
                                    // The settlement cannot finish:
                                    // the T1-durable obligation is
                                    // EXPLICITLY left for the
                                    // crash-recovery re-drive.
                                    ReconcileOutcome::NotInstalled => Ok(false),
                                    ReconcileOutcome::Unverifiable { .. } => {
                                        Err(TransitionError::InstallIncompatible {
                                            region: 0,
                                            detail: "maintenance/unverifiable-attempt",
                                        })
                                    }
                                }
                            }
                            CommitOutcome::StaleParent { .. } => Ok(false),
                        }
                    }
                    ReconcileOutcome::Unverifiable { .. } => {
                        Err(TransitionError::InstallIncompatible {
                            region: 0,
                            detail: "maintenance/unverifiable-attempt",
                        })
                    }
                }
            }
            CommitOutcome::StaleParent { .. } => Ok(false),
        }
    }

    /// The close gate's failure classification: a dead worker is the
    /// unavailable class; the two unverifiable-reconciliation details
    /// the unverify class; every other authority refusal carries its
    /// detail.
    fn close_gate_failure(&self, error: TransitionError) -> CloseFailure {
        if !self.worker.healthy() {
            return CloseFailure::WorkerUnavailable;
        }
        match &error {
            TransitionError::InstallIncompatible { detail, .. } => {
                if *detail == "maintenance/unverifiable-attempt"
                    || *detail == "maintenance/unverifiable-publication"
                {
                    CloseFailure::UnverifiableReconciliation
                } else {
                    CloseFailure::Authority((*detail).to_owned())
                }
            }
            other => CloseFailure::Authority(gate_detail(other)),
        }
    }

    /// THE FINAL REQUIRED SNAPSHOT (the §6 steps 7-10): a FRESH
    /// capture each attempt (the newest authoritative state — never
    /// an earlier queued snapshot, never rebased bytes), the
    /// REQUIRED blocking admission, the verified readback; a stale
    /// captured parent recaptures and retries under the bounded
    /// discipline; an uncertain disposition reconciles (Installed →
    /// the verified acceptance with the readback; NotInstalled →
    /// the recapture retry; Unverifiable → the failed close).
    fn publish_final_snapshot(&mut self) -> Result<(Hash, SyncTier), CloseFailure> {
        const FINAL_CLOSE_ATTEMPTS: usize = 3;
        for _ in 0..FINAL_CLOSE_ATTEMPTS {
            let capture = self
                .capture_snapshot()
                .map_err(|error| CloseFailure::PublicationFailed(gate_detail(&error)))?;
            // THE IDEMPOTENT FINAL SAVE (C4-F1): the exact current
            // serialized state may already be represented by an
            // eligible chain-reachable revision (an accepted optional
            // snapshot of this unchanged state — real sessions close
            // immediately after a save). A genuinely-required new
            // publication of equivalent bytes is IMPOSSIBLE under the
            // ratified idempotency contract (the key IS the content
            // hash, unique across the chain for the store's
            // lifetime): the obligation is therefore satisfied by the
            // existing representation, INDEPENDENTLY VERIFIED —
            // never a duplicate-key failure for a state already
            // durably represented, never rebased bytes, no artificial
            // clock advance, no persistent-format change.
            if let Some(existing) = self.find_representing_snapshot(&capture)? {
                self.verify_representing_snapshot(&existing, &capture.content)?;
                return Ok((existing, self.worker.tier()));
            }
            let record =
                RevisionRecord::regular(capture.parent, capture.parent_epoch, capture.tick)
                    .with_idempotency_key(capture.content.as_bytes().to_vec())
                    .with_content_refs(vec![capture.content]);
            let revision = record.id();
            let slot = self
                .worker
                .submit_required(MediumOp::Append {
                    record,
                    objects: vec![capture.object],
                    expected_parent: Some(capture.parent),
                })
                .map_err(|_| CloseFailure::WorkerUnavailable)?;
            match slot.recv() {
                // C4-F2: a lost reply on an ADMITTED final append is
                // an UNCERTAIN candidate, never proof of failure —
                // the append may have committed with the worker still
                // healthy (the accepted lost-reply class), or the
                // worker may be dead (the fail-closed class). The
                // candidate's exact revision, parent, content, and
                // capture provenance are all in scope here.
                Err(_) => {
                    if !self.worker.healthy() {
                        return Err(CloseFailure::WorkerUnavailable);
                    }
                    let mut medium = self
                        .worker
                        .source()
                        .map_err(|_| CloseFailure::WorkerUnavailable)?
                        .open_medium()
                        .map_err(|_| CloseFailure::WorkerUnavailable)?;
                    match medium
                        .reconcile_unknown(&revision)
                        .map_err(|error| CloseFailure::PublicationFailed(error.to_string()))?
                    {
                        Reconciliation::Installed { id } => {
                            // The full final-publication readback on
                            // the VERIFIED installation.
                            self.verify_final_publication(&id, &capture.parent, &capture.content)?;
                            return Ok((id, self.worker.tier()));
                        }
                        // The candidate genuinely did not install:
                        // the fresh recapture/retry (no duplicate
                        // installation).
                        Reconciliation::NotInstalled { .. } => continue,
                        Reconciliation::Unverifiable { .. } => {
                            return Err(CloseFailure::UnverifiableReconciliation);
                        }
                    }
                }
                Ok(Err(error)) => {
                    return Err(match error {
                        MediumError::WorkerUnavailable { .. } => CloseFailure::WorkerUnavailable,
                        other => CloseFailure::PublicationFailed(other.to_string()),
                    });
                }
                Ok(Ok(WorkerReply::Append(outcome))) => match outcome {
                    AppendOutcome::Accepted { id, tier } => {
                        // (9) THE READBACK VERIFICATION.
                        self.verify_final_publication(&id, &capture.parent, &capture.content)?;
                        return Ok((id, tier));
                    }
                    // (10) THE STALE CAPTURED PARENT: recapture and
                    // retry — the bytes are never rebased.
                    AppendOutcome::RejectedStaleParent { .. } => continue,
                    AppendOutcome::Unknown { .. } => {
                        let mut medium = self
                            .worker
                            .source()
                            .map_err(|_| CloseFailure::WorkerUnavailable)?
                            .open_medium()
                            .map_err(|_| CloseFailure::WorkerUnavailable)?;
                        match medium
                            .reconcile_unknown(&revision)
                            .map_err(|error| CloseFailure::PublicationFailed(error.to_string()))?
                        {
                            Reconciliation::Installed { id } => {
                                self.verify_final_publication(
                                    &id,
                                    &capture.parent,
                                    &capture.content,
                                )?;
                                return Ok((id, self.worker.tier()));
                            }
                            Reconciliation::NotInstalled { .. } => continue,
                            Reconciliation::Unverifiable { .. } => {
                                return Err(CloseFailure::UnverifiableReconciliation);
                            }
                        }
                    }
                },
                Ok(Ok(other)) => unreachable!("the final publication reply was {other:?}"),
            }
        }
        Err(CloseFailure::RecaptureExhausted)
    }

    /// The final publication's READBACK VERIFICATION (§6.9): the
    /// installed record and its content object are read back through
    /// the single worker and verified against the capture — the
    /// record's parent and content reference, and the object's
    /// content identity.
    fn verify_final_publication(
        &mut self,
        revision: &Hash,
        parent: &Hash,
        content: &Hash,
    ) -> Result<(), CloseFailure> {
        let mut medium = self
            .worker
            .source()
            .map_err(|_| CloseFailure::WorkerUnavailable)?
            .open_medium()
            .map_err(|_| CloseFailure::WorkerUnavailable)?;
        let record = medium
            .read_record(revision)
            .map_err(|error| CloseFailure::ReadbackFailed(error.to_string()))?;
        if record.parent != Some(*parent) || record.content_refs != vec![*content] {
            return Err(CloseFailure::ReadbackFailed(
                "the installed record does not bind the captured parent and content".to_owned(),
            ));
        }
        let bytes = medium
            .read_object(content)
            .map_err(|error| CloseFailure::ReadbackFailed(error.to_string()))?;
        let object = StoredObject::new(bytes)
            .map_err(|error| CloseFailure::ReadbackFailed(error.to_string()))?;
        if object.hash() != *content {
            return Err(CloseFailure::ReadbackFailed(
                "the content object's identity does not verify".to_owned(),
            ));
        }
        Ok(())
    }

    /// THE IDEMPOTENCE WALK (C4-F1 + C4-R2 + C4-R4): searches the
    /// RECENT chain window for the NEWEST revision already carrying
    /// the exact current snapshot content — satisfying the idempotent
    /// close ONLY if it remains the LATEST recoverable session
    /// snapshot under the accepted recovery contract
    /// (`latest_snapshot_at` selects the newest recognizable session
    /// snapshot): an intervening DIFFERENT session snapshot — of ANY
    /// recognizable version, determined by the ACCEPTED recovery
    /// recognizer `is_session_object` (the domain family: a newer
    /// version this build cannot restore supersedes exactly like a
    /// different supported one, because recovery fails closed on the
    /// newer snapshot — never a success claimed from an older
    /// revision below it) — supersedes the older match (the typed
    /// conflict — the current state cannot be made authoritative
    /// without violating the chain-wide idempotency rule), while an
    /// intervening NON-SNAPSHOT canonical revision (a confirmation
    /// batch, an MK, an audit) does not. The walk descends from the
    /// canonical head; a reclamation gap ends the window (the check
    /// falls through to the publication path, which handles its own
    /// outcomes honestly — the residual bounded-search case where a
    /// match lies beyond the window surfaces there as the provider's
    /// duplicate-key refusal, never a false success); the founding
    /// ends it; read failures other than the gap class fail the
    /// close closed — never a guessed satisfaction.
    fn find_representing_snapshot(
        &self,
        capture: &SnapshotCapture,
    ) -> Result<Option<Hash>, CloseFailure> {
        const REPRESENTATION_WINDOW: usize = 64;
        let mut medium = self
            .worker
            .source()
            .map_err(|_| CloseFailure::WorkerUnavailable)?
            .open_medium()
            .map_err(|_| CloseFailure::WorkerUnavailable)?;
        let Some(mut cursor) = medium
            .head()
            .map_err(|error| CloseFailure::ReadbackFailed(error.to_string()))?
        else {
            return Ok(None);
        };
        let mut superseded = false;
        for _ in 0..REPRESENTATION_WINDOW {
            let record = match medium.read_record(&cursor) {
                Ok(record) => record,
                // A reclamation gap ends the recent window — the
                // check falls through to the publication path.
                Err(MediumError::MissingObject { .. }) => return Ok(None),
                Err(other) => return Err(CloseFailure::ReadbackFailed(other.to_string())),
            };
            // THE RECOVERY-ANCHOR CLASSIFICATION (C4-R2 + C4-R4): an
            // intervening record is either a session snapshot —
            // recognized by the ACCEPTED recovery recognizer's
            // domain FAMILY (`is_session_object`, any version: a
            // different supported snapshot AND a recognizable
            // future version this build cannot restore both
            // supersede any older match, because recovery fails
            // closed on the newer one) — or a non-snapshot canonical
            // revision (a batch, an MK, an audit — no supersession).
            // Only the exact supported `/1` domain can carry the
            // current content (the capture is a `/1` serialization
            // by construction — the hash equality proves the bytes).
            if let Some(content_hash) = record.content_refs.first() {
                let bytes = medium
                    .read_object(content_hash)
                    .map_err(|error| CloseFailure::ReadbackFailed(error.to_string()))?;
                if is_session_object(&bytes) {
                    if let Ok(domain) = peek_object_domain(&bytes)
                        && domain == SESSION_SNAPSHOT_DOMAIN
                        && record.content_refs == vec![capture.content]
                    {
                        // The newest revision carrying the exact
                        // current content — satisfying the close ONLY
                        // if it is also the latest recoverable
                        // session snapshot.
                        if superseded {
                            return Err(CloseFailure::SupersededSnapshot);
                        }
                        return Ok(Some(cursor));
                    }
                    superseded = true;
                }
            }
            let Some(parent) = record.parent else {
                return Ok(None);
            };
            cursor = parent;
        }
        Ok(None)
    }

    /// The idempotent final save's VERIFICATION (C4-F1): the found
    /// revision is read back and verified — its content reference
    /// binds the exact current content, the content object IS a
    /// session snapshot (the ratified domain), and the object's
    /// identity verifies — before the existing representation
    /// satisfies the final-save obligation. Under the
    /// single-submitter discipline an unchanged serialized state
    /// implies an unchanged journal frontier (only deactivations
    /// grow the committed set, and every deactivation changes the
    /// state), so the content equality carries the frontier
    /// applicability.
    fn verify_representing_snapshot(
        &self,
        revision: &Hash,
        content: &Hash,
    ) -> Result<(), CloseFailure> {
        let mut medium = self
            .worker
            .source()
            .map_err(|_| CloseFailure::WorkerUnavailable)?
            .open_medium()
            .map_err(|_| CloseFailure::WorkerUnavailable)?;
        let record = medium
            .read_record(revision)
            .map_err(|error| CloseFailure::ReadbackFailed(error.to_string()))?;
        if record.content_refs != vec![*content] {
            return Err(CloseFailure::ReadbackFailed(
                "the representing revision does not bind the current content".to_owned(),
            ));
        }
        let bytes = medium
            .read_object(content)
            .map_err(|error| CloseFailure::ReadbackFailed(error.to_string()))?;
        let object = StoredObject::new(bytes)
            .map_err(|error| CloseFailure::ReadbackFailed(error.to_string()))?;
        if object.hash() != *content {
            return Err(CloseFailure::ReadbackFailed(
                "the content object's identity does not verify".to_owned(),
            ));
        }
        Ok(())
    }

    // ----------------------------------------------------------- internals

    /// The observed canonical head and its epoch (a fresh read through
    /// the single worker).
    fn observe_head(&self) -> Result<(Hash, u64), TransitionError> {
        let mut medium = self
            .worker
            .source()
            .map_err(PipelineError::Medium)?
            .open_medium()
            .map_err(PipelineError::Medium)?;
        let head = medium
            .head()
            .map_err(PipelineError::Medium)?
            .ok_or(PipelineError::UnfoundedMedium)?;
        let epoch = medium
            .read_record(&head)
            .map_err(PipelineError::Medium)?
            .epoch;
        Ok((head, epoch))
    }

    /// The structured consequential coverage from the director's
    /// ledger — the same authoritative state the serialized bytes
    /// represent.
    fn coverage_frontier(&self) -> CoverageFrontier {
        let ledger = self.director.ledger();
        let mut materialized = vec![0u64; NPC_COUNT];
        for (id, amount) in ledger.applied_entries() {
            materialized[id.npc as usize] = materialized[id.npc as usize].saturating_add(amount);
        }
        let (lane_boundary, carried) = ledger
            .summarized()
            .map(|summary| (summary.boundary, summary.applied_totals.clone()))
            .unwrap_or(([0; REGION_COUNT], vec![0; NPC_COUNT]));
        for (npc, total) in carried.iter().enumerate() {
            materialized[npc] = materialized[npc].saturating_add(*total);
        }
        let pending = ledger.pending_entries().map(|(id, _)| id).collect();
        CoverageFrontier {
            materialized,
            lane_boundary,
            pending,
        }
    }

    /// Retires one pending entry into the completion ledger (the
    /// shared record construction — the poll/wait completions, the
    /// maintenance harvest, and the join/close harvests).
    fn retire_entry(&mut self, entry: PendingPublication, disposition: CompletionDisposition) {
        self.completed.push(CompletionRecord {
            id: entry.id,
            revision: entry.revision,
            disposition,
            coverage: entry.coverage,
            journal_frontier: entry.journal_frontier,
        });
    }

    /// Completes the pending entry at `index` with the worker's reply,
    /// retaining the completion record (the candidate identity, the
    /// TYPED DISPOSITION, and the captured frontier — the record
    /// survives the caller's consumption of the returned outcome) and
    /// returning the outcome.
    fn complete_at(
        &mut self,
        index: usize,
        reply: Result<WorkerReply, MediumError>,
    ) -> PublicationOutcome {
        let entry = self.pending.remove(index);
        let outcome = publication_outcome(entry.revision, reply);
        let disposition = CompletionDisposition::of(&outcome);
        self.retire_entry(entry, disposition);
        outcome
    }

    /// The disconnected-slot completion: the worker died with the job
    /// in flight — the possibly executed append's disposition is
    /// UNKNOWN (never interpreted), the candidate identity retained.
    fn unknown_at(&mut self, index: usize) -> PublicationOutcome {
        let entry = self.pending.remove(index);
        let revision = entry.revision;
        self.retire_entry(
            entry,
            CompletionDisposition::Unknown {
                reason: UnknownReason::WorkerReplyLost,
            },
        );
        PublicationOutcome::Unknown {
            revision,
            reason: UnknownReason::WorkerReplyLost,
        }
    }
}
