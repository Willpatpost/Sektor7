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

/// One retained completion record: the candidate identity and the
/// captured frontier survive the completion (the accounting never
/// depends on the caller's polling).
#[derive(Debug, Clone)]
pub struct CompletionRecord {
    /// The publication's orchestrator-local id.
    pub id: u64,
    /// The candidate's identity (the publication record's id).
    pub revision: Hash,
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
    /// accounting).
    pending: Vec<PendingPublication>,
    /// The retained completion records (bounded by the queue capacity
    /// plus the in-flight window).
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
        let scan = T1Journal::scan_file(&self.journal_root).map_err(PipelineError::Journal)?;
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

    /// Joins the worker (the clean drain — every source and proxy
    /// drops with the session).
    pub fn join(self) -> Result<(), MediumError> {
        self.worker.join()
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
    /// retaining the completion record (the candidate identity and the
    /// captured frontier) and returning the outcome.
    fn complete_at(
        &mut self,
        index: usize,
        reply: Result<WorkerReply, MediumError>,
    ) -> PublicationOutcome {
        let entry = self.pending.remove(index);
        self.completed.push(CompletionRecord {
            id: entry.id,
            revision: entry.revision,
            coverage: entry.coverage,
            journal_frontier: entry.journal_frontier,
        });
        match reply {
            Ok(WorkerReply::Append(AppendOutcome::Accepted { id, tier })) => {
                PublicationOutcome::Accepted { revision: id, tier }
            }
            Ok(WorkerReply::Append(AppendOutcome::RejectedStaleParent { observed })) => {
                PublicationOutcome::Stale {
                    revision: entry.revision,
                    observed,
                }
            }
            Ok(WorkerReply::Append(AppendOutcome::Unknown { reason })) => {
                PublicationOutcome::Unknown {
                    revision: entry.revision,
                    reason,
                }
            }
            Ok(WorkerReply::Canceled) => PublicationOutcome::Canceled {
                revision: entry.revision,
            },
            Ok(other) => unreachable!("the publication reply was {other:?}"),
            Err(error) => PublicationOutcome::Failed {
                revision: entry.revision,
                error,
            },
        }
    }

    /// The disconnected-slot completion: the worker died with the job
    /// in flight — the possibly executed append's disposition is
    /// UNKNOWN (never interpreted), the candidate identity retained.
    fn unknown_at(&mut self, index: usize) -> PublicationOutcome {
        let entry = self.pending.remove(index);
        self.completed.push(CompletionRecord {
            id: entry.id,
            revision: entry.revision,
            coverage: entry.coverage,
            journal_frontier: entry.journal_frontier,
        });
        PublicationOutcome::Unknown {
            revision: entry.revision,
            reason: UnknownReason::WorkerReplyLost,
        }
    }
}
