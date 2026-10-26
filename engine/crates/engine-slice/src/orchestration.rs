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
//! as Checkpoint 4's maintenance drain will.
//!
//! NO maintenance entry points (rotation/reclamation orchestration
//! and the graceful-shutdown final-save belong to Checkpoint 4) and
//! no new path around the future maintenance admission lease.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvError, TryRecvError};

use engine_core::{EventId, EventSpec, LodLevel, PresenceSample, ScheduleError, Spine, Tick};
use engine_persist::{
    AppendOutcome, CanonicalMedium, CommitOutcome, Hash, MediumError, PipelineError,
    RevisionRecord, StoredObject, SyncTier, T1Journal, TransactionId, UnknownReason,
    active_journal_path,
};

use crate::crossing::{SessionCrossing, TransitionError, save_session};
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
        self.completed.push(CompletionRecord {
            id: entry.id,
            revision: entry.revision,
            disposition: CompletionDisposition::of(&outcome),
            coverage: entry.coverage,
            journal_frontier: entry.journal_frontier,
        });
        outcome
    }

    /// The disconnected-slot completion: the worker died with the job
    /// in flight — the possibly executed append's disposition is
    /// UNKNOWN (never interpreted), the candidate identity retained.
    fn unknown_at(&mut self, index: usize) -> PublicationOutcome {
        let entry = self.pending.remove(index);
        let outcome = PublicationOutcome::Unknown {
            revision: entry.revision,
            reason: UnknownReason::WorkerReplyLost,
        };
        self.completed.push(CompletionRecord {
            id: entry.id,
            revision: entry.revision,
            disposition: CompletionDisposition::Unknown {
                reason: UnknownReason::WorkerReplyLost,
            },
            coverage: entry.coverage,
            journal_frontier: entry.journal_frontier,
        });
        outcome
    }
}
