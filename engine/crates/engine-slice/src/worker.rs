//! PP-B4 checkpoint 2: the campaign's single canonical I/O worker.
//!
//! One dedicated thread per active campaign owns the ONE `SessionStore`
//! connection (the only provider open — opened at spawn, moved into the
//! thread; the proxies never open another) and executes jobs FIFO from
//! one bounded queue. The [`WorkerMedium`] proxy implements
//! [`CanonicalMedium`] by sending each operation as a job and blocking
//! on its private one-slot reply channel: a reply is deposited without
//! a reader, so an abandoned receiver is a typed send failure the
//! worker ignores — it never blocks on a dead caller. The
//! [`WorkerSource`] is the worker-backed [`MediumSource`]: every handle
//! it opens is a queue proxy, never a provider connection.
//!
//! THE FAILURE DISCIPLINE (the preflight's accepted outcome table):
//! an admission failure (the queue's receiver is gone) and any
//! side-effect-free read's transport loss are DEFINITE for the request
//! they name, typed [`MediumError::WorkerUnavailable`], and poison the
//! session; but a lost reply on an APPEND is never a definite failure
//! — the append may have committed, so the proxy synthesizes
//! `AppendOutcome::Unknown { reason: WorkerReplyLost }`, preserving the
//! candidate's identity for reconciliation. Worker death (any exit
//! path, including a panic unwind) flips the shared health cell
//! instantly — readable without a blocking request — and every
//! authoritative crossing operation refuses through the source's
//! `alive` gate: the session is poisoned, never silently restarted;
//! fresh recovery reestablishes trustworthy state.
//!
//! The queue capacity is a PROVISIONAL engineering default (64), not a
//! ratified performance target.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use engine_persist::{
    AppendOutcome, CanonicalMedium, ChainSummary, Hash, MediumError, ReclaimUnit, Reconciliation,
    RevisionRecord, StoredObject, SyncTier, UnknownReason,
};

use crate::store::{MediumSource, SessionStore, StoreLocation};

/// The provisional queue-capacity default (an engineering default per
/// the GO, not a ratified performance target).
pub const DEFAULT_QUEUE_CAPACITY: usize = 64;

// ------------------------------------------------------------ the protocol

/// One queued canonical-medium operation. `Send` by construction —
/// the payload types are the engine's plain value types (the
/// checkpoint-1 portability record).
pub enum MediumOp {
    Head,
    Append {
        record: RevisionRecord,
        objects: Vec<StoredObject>,
        expected_parent: Option<Hash>,
    },
    ReadRecord {
        id: Hash,
    },
    ReadObject {
        hash: Hash,
    },
    Validate,
    ReconcileUnknown {
        candidate: Hash,
    },
    Reclaim {
        units: Vec<ReclaimUnit>,
    },
    ReclaimPending {
        units: Vec<ReclaimUnit>,
    },
    Checkpoint,
    /// The maintenance FIFO barrier (PP-B4 checkpoint 4): a no-op
    /// job whose reply, by FIFO position, proves every previously
    /// admitted canonical job executed AND replied — the maintenance
    /// lease's settlement evidence.
    Barrier,
}

/// The worker's reply for one executed operation.
#[derive(Debug)]
pub enum WorkerReply {
    Head(Option<Hash>),
    Append(AppendOutcome),
    ReadRecord(RevisionRecord),
    ReadObject(Vec<u8>),
    Validate(ChainSummary),
    Reconcile(Reconciliation),
    Reclaim(u64),
    ReclaimPending(bool),
    Checkpoint,
    /// A queued job was canceled before it started (the optional-
    /// publication token, PP-B4 checkpoint 3) — a definite non-event:
    /// nothing was executed.
    Canceled,
    /// The maintenance barrier's no-op reply (PP-B4 checkpoint 4):
    /// every previously admitted job has executed and replied.
    Barrier,
}

/// One queued job with its private bounded completion slot: exactly
/// one reply is deposited per slot (capacity 1 — the worker's send
/// never blocks); an abandoned receiver makes that send fail, which
/// the worker ignores.
struct Job {
    op: MediumOp,
    /// The optional-publication cancellation token (PP-B4 checkpoint 3):
    /// checked ONCE before execution — a canceled queued job is skipped
    /// (a definite non-event); a job already executing runs to its
    /// completion boundary regardless.
    cancel: Option<Arc<AtomicBool>>,
    reply: SyncSender<Result<WorkerReply, MediumError>>,
}

/// One job's reply channel (the bounded completion slot): the worker
/// deposits exactly one result (capacity 1 — the send never blocks;
/// an abandoned receiver is a typed send failure the worker ignores).
type ReplySlot = (
    SyncSender<Result<WorkerReply, MediumError>>,
    Receiver<Result<WorkerReply, MediumError>>,
);

fn completion_slot() -> ReplySlot {
    sync_channel(1)
}

// -------------------------------------------------------- the fault control

/// The job kinds the after-faults key on.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Append,
    Reconcile,
    ReadObject,
}

/// Test-only deterministic fault control — PER WORKER INSTANCE (the
/// established per-instance seam pattern of `JournalFaultControl`; no
/// process-global statics, so parallel tests never collide).
#[doc(hidden)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WorkerFault {
    /// No fault armed.
    #[default]
    None,
    /// Panic before executing the next job (the thread dies; the
    /// health cell flips through the drop guard).
    PanicNext,
    /// Execute the next job of this kind NORMALLY, then panic before
    /// the job that follows it (the death lands inside the caller's
    /// readback phase).
    PanicAfter(JobKind),
    /// Sleep before each job while armed (the stall used to observe
    /// serialization and bounded-queue behavior).
    StallEach { ms: u64 },
    /// The next append returns a DEFINITE pre-commit error without
    /// touching the store (the medium's own pre-commit class, relayed
    /// — nothing installed; the direct-retry contract applies).
    FailAppendPreCommit,
    /// The next append EXECUTES against the store (it may commit) but
    /// its reply is never sent — the proxy must surface
    /// `Unknown { WorkerReplyLost }`.
    LoseAppendReply,
    /// The next append executes, then the process hard-exits (exit
    /// 13) before the reply — the real-child after-commit arm.
    ExitAfterAppendPreReply,
    /// The process hard-exits (exit 14) before executing the next job
    /// — the real-child definite never-started arm.
    ExitBeforeNextJob,
    /// Park the worker BEFORE executing the next job until
    /// `release_park` — the deterministic synchronization barrier (the
    /// timed stall's replacement).
    ParkNext,
    /// Park INSIDE the append path, AFTER the poison and cancellation
    /// boundaries — the job is already executing (the
    /// poison-during-executing-append case).
    ParkExecutingAppend,
    /// The next append job is SKIPPED (it never touches the store)
    /// and its reply dropped, the worker staying healthy — the
    /// lost-reply-with-a-genuinely-not-installed-candidate case for
    /// the close's required final publication (C4-F2's regression D:
    /// the reconciliation must prove the absence, never guess).
    DropAppendWithoutExecution,
    /// The next `count` append jobs are SKIPPED (they never touch the
    /// store) with their replies dropped, the worker staying healthy
    /// — the MULTI-SHOT definite-not-installed seam (each skipped
    /// append decrements the count; zero disarms): the close's
    /// bounded settlement discipline proven at its honest failure
    /// boundary (C4-R3's regression D — the first required submission
    /// AND its bounded retry both verified-absent).
    DropNextAppends { count: u32 },
    /// The next ReconcileUnknown job is NOT executed against the
    /// store — the worker synthesizes the typed `Unverifiable`
    /// outcome as its reply (one-shot, the worker healthy): the
    /// deterministic end-to-end seam for the maintenance/close
    /// gates' unverifiable-reconciliation refusals (the final
    /// validation checkpoint's evidence-limitation A — no production
    /// API widened, the provider's own fault machinery untouched).
    UnverifiableNextReconcile,
    /// The next Checkpoint job PANICS before execution (the thread
    /// dies; the request fails with the worker-dead error) — the
    /// death lands between the verified final publication and the
    /// lifecycle checkpoint: the checkpoint-failure report-contract
    /// seam (the C4-R2 walk's content-object reads invalidated the
    /// earlier ReadObject-timing construction).
    PanicBeforeCheckpoint,
}

/// The shared per-instance fault setter/getter, plus the one-shot park
/// latch the park faults wait on.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct WorkerFaultControl {
    fault: Arc<Mutex<WorkerFault>>,
    park: Arc<ParkLatch>,
}

/// The park latch (the deterministic synchronization barrier): parks
/// until released — one-shot (each test uses a fresh control; the
/// release is never re-armed). The ENTRY flag flips when the worker
/// parks (before waiting) — the deterministic replacement for
/// timing-based "the worker holds the job" assumptions (the
/// final-validation checkpoint's determinism audit: the tests wait on
/// the actual event, never a guessed duration).
#[doc(hidden)]
#[derive(Default)]
struct ParkLatch {
    entered: AtomicBool,
    released: Mutex<bool>,
    notified: Condvar,
}

impl WorkerFaultControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, fault: WorkerFault) {
        *self.fault.lock().expect("the worker fault lock") = fault;
    }

    pub fn clear(&self) {
        *self.fault.lock().expect("the worker fault lock") = WorkerFault::None;
    }

    /// Releases the park latch — the barrier's open signal (every
    /// parked worker wakes).
    pub fn release_park(&self) {
        let mut released = self.park.released.lock().expect("the worker park latch");
        *released = true;
        self.park.notified.notify_all();
    }

    /// Whether the worker has ENTERED the park — the deterministic
    /// "the job has started" barrier (wait on this instead of sleeping
    /// a guessed duration; the flag flips before the parked wait).
    #[doc(hidden)]
    pub fn park_entered(&self) -> bool {
        self.park.entered.load(Ordering::Acquire)
    }

    fn current(&self) -> WorkerFault {
        self.fault.lock().expect("the worker fault lock").clone()
    }

    fn park_until_released(&self) {
        self.park.entered.store(true, Ordering::Release);
        let mut released = self.park.released.lock().expect("the worker park latch");
        while !*released {
            released = self
                .park
                .notified
                .wait(released)
                .expect("the worker park latch");
        }
    }
}

/// The typed nonblocking-admission refusal for an optional operation
/// (PP-B4 checkpoint 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrySubmitError {
    /// The bounded queue is at capacity — nothing was admitted (no
    /// append); retry or regenerate at the next boundary.
    QueueFull,
    /// The worker is unavailable (poisoned or exited) — nothing was
    /// admitted.
    Unavailable,
}

// ------------------------------------------------------------- the health

const HEALTH_ALIVE: u8 = 0;
const HEALTH_DEAD: u8 = 1;

/// The health cell shared between the worker thread, the handle, the
/// source, and the crossing's authority gate: `ALIVE` until any exit
/// path — normal drain, or a panic unwind — flips `DEAD` through the
/// thread's drop guard. Readable without a blocking request.
#[derive(Clone)]
struct Health(Arc<AtomicU8>);

impl Health {
    fn alive(&self) -> bool {
        self.0.load(Ordering::Relaxed) == HEALTH_ALIVE
    }

    fn dead(&self) {
        self.0.store(HEALTH_DEAD, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------- the worker loop

/// Flips the shared health cell to DEAD on ANY exit path — including
/// a panic unwind (the guard drops during unwinding).
struct DeathGuard(Health);

impl Drop for DeathGuard {
    fn drop(&mut self) {
        self.0.dead();
    }
}

fn worker_loop(
    rx: Receiver<Job>,
    mut store: SessionStore,
    health: Health,
    fault: WorkerFaultControl,
) {
    let _guard = DeathGuard(health.clone());
    for job in rx {
        match fault.current() {
            WorkerFault::None
            | WorkerFault::PanicAfter(_)
            | WorkerFault::FailAppendPreCommit
            | WorkerFault::LoseAppendReply
            | WorkerFault::DropAppendWithoutExecution
            | WorkerFault::DropNextAppends { .. }
            | WorkerFault::UnverifiableNextReconcile
            | WorkerFault::ExitAfterAppendPreReply => {}
            WorkerFault::PanicNext => {
                panic!("canonical-io-worker: injected fault (PanicNext)")
            }
            WorkerFault::ExitBeforeNextJob => std::process::exit(14),
            WorkerFault::StallEach { ms } => thread::sleep(Duration::from_millis(ms)),
            WorkerFault::ParkNext => fault.park_until_released(),
            WorkerFault::ParkExecutingAppend => {}
            WorkerFault::PanicBeforeCheckpoint => {
                if let MediumOp::Checkpoint = job.op {
                    fault.clear();
                    panic!("canonical-io-worker: injected fault (PanicBeforeCheckpoint)");
                }
            }
        }
        // THE POISON EXECUTION BOUNDARY (C2-F2): a job admitted before the
        // poison but not yet started is refused HERE with the DEFINITE typed
        // error (the refusal is deterministic — the job never ran, never the
        // uncertain Unknown class), while a job that already began executing
        // completes normally. The poisoned worker keeps draining — refuse
        // and reply — so stop/join remain clean.
        if !health.alive() {
            let _ = job.reply.send(Err(MediumError::WorkerUnavailable {
                detail:
                    "the I/O worker is poisoned — the job was refused at the execution boundary"
                        .to_owned(),
            }));
            continue;
        }
        // THE CANCELLATION BOUNDARY (PP-B4 checkpoint 3): a queued
        // publication canceled before start is skipped — a DEFINITE
        // non-event (no append); a job already past this point runs to
        // its completion boundary regardless (a possibly committed
        // outcome is never erased).
        if let Some(cancel) = job.cancel.as_ref()
            && cancel.load(Ordering::Relaxed)
        {
            let _ = job.reply.send(Ok(WorkerReply::Canceled));
            continue;
        }
        if let MediumOp::Append { .. } = job.op {
            match fault.current() {
                WorkerFault::FailAppendPreCommit => {
                    fault.clear();
                    let _ = job.reply.send(Err(MediumError::Io {
                        op: "io-worker append (injected pre-commit fault)",
                        path: String::new(),
                        source: std::io::Error::other("injected pre-commit failure"),
                    }));
                    continue;
                }
                WorkerFault::LoseAppendReply => {
                    fault.clear();
                    // The append EXECUTES (it may commit); the reply is never
                    // sent — the caller must reconcile the uncertain class.
                    let _ = execute(&mut store, job.op);
                    drop(job.reply);
                    continue;
                }
                WorkerFault::DropAppendWithoutExecution => {
                    fault.clear();
                    // The append NEVER EXECUTES (nothing installed) and the
                    // reply is dropped, the worker staying healthy — the
                    // caller must reconcile the uncertain class to the
                    // VERIFIED absence (C4-F2's regression D).
                    drop(job.reply);
                    continue;
                }
                WorkerFault::DropNextAppends { count } => {
                    if count <= 1 {
                        fault.clear();
                    } else {
                        fault.set(WorkerFault::DropNextAppends { count: count - 1 });
                    }
                    // Each armed append NEVER EXECUTES (nothing installed)
                    // and its reply is dropped, the worker staying healthy —
                    // the caller must reconcile the uncertain class to the
                    // VERIFIED absence (the count disarms at zero).
                    drop(job.reply);
                    continue;
                }
                WorkerFault::ExitAfterAppendPreReply => {
                    let _ = execute(&mut store, job.op);
                    std::process::exit(13);
                }
                // Park INSIDE the executing append — past the poison and
                // cancellation boundaries (the job has started).
                WorkerFault::ParkExecutingAppend => fault.park_until_released(),
                _ => {}
            }
        }
        // THE UNVERIFIABLE RECONCILIATION SEAM (the final-validation
        // checkpoint's evidence limitation A): the next
        // ReconcileUnknown job is NOT executed — the typed
        // `Unverifiable` outcome is synthesized as the reply, exactly
        // what a provider could not establish. The caller's gate must
        // refuse on it, never converting the uncertainty into a
        // confirmation (Installed) or a verified absence
        // (NotInstalled) — the isolated per-worker test-only
        // passthrough, no production API widened.
        if let MediumOp::ReconcileUnknown { .. } = job.op
            && matches!(fault.current(), WorkerFault::UnverifiableNextReconcile)
        {
            fault.clear();
            let _ = job
                .reply
                .send(Ok(WorkerReply::Reconcile(Reconciliation::Unverifiable {
                    reason: "injected test fault (UnverifiableNextReconcile)".to_owned(),
                })));
            continue;
        }
        let executed_kind = job_kind(&job.op);
        let reply = execute(&mut store, job.op);
        // An abandoned receiver fails this send; the worker ignores it and
        // never blocks (one reply per one-slot channel).
        let _ = job.reply.send(reply);
        if let Some(kind) = executed_kind
            && matches!(fault.current(), WorkerFault::PanicAfter(armed) if armed == kind)
        {
            fault.set(WorkerFault::PanicNext);
        }
    }
}

fn execute(store: &mut SessionStore, op: MediumOp) -> Result<WorkerReply, MediumError> {
    match op {
        MediumOp::Head => store.head().map(WorkerReply::Head),
        MediumOp::Append {
            record,
            objects,
            expected_parent,
        } => store
            .append(&record, &objects, expected_parent)
            .map(WorkerReply::Append),
        MediumOp::ReadRecord { id } => store.read_record(&id).map(WorkerReply::ReadRecord),
        MediumOp::ReadObject { hash } => store.read_object(&hash).map(WorkerReply::ReadObject),
        MediumOp::Validate => store.validate().map(WorkerReply::Validate),
        MediumOp::ReconcileUnknown { candidate } => store
            .reconcile_unknown(&candidate)
            .map(WorkerReply::Reconcile),
        MediumOp::Reclaim { units } => store.reclaim(&units).map(WorkerReply::Reclaim),
        MediumOp::ReclaimPending { units } => store
            .reclaim_pending(&units)
            .map(WorkerReply::ReclaimPending),
        MediumOp::Checkpoint => store.checkpoint().map(|()| WorkerReply::Checkpoint),
        MediumOp::Barrier => Ok(WorkerReply::Barrier),
    }
}

fn job_kind(op: &MediumOp) -> Option<JobKind> {
    match op {
        MediumOp::Append { .. } => Some(JobKind::Append),
        MediumOp::ReconcileUnknown { .. } => Some(JobKind::Reconcile),
        MediumOp::ReadObject { .. } => Some(JobKind::ReadObject),
        _ => None,
    }
}

// ----------------------------------------------------------- the proxy

/// The queue-proxy [`CanonicalMedium`]: every operation is one job on
/// the campaign's single queue; the handle is a channel sender, NEVER
/// a provider connection. The cached [`SyncTier`] was read from the
/// actual provider at spawn and is reported synchronously, truthfully,
/// with no worker round trip. The shared health cell enforces the
/// terminal-poison admission contract (C2-F2): a poisoned or exited
/// worker refuses every NEW operation at the handle itself — EXISTING
/// proxy handles included, not only newly created ones.
pub struct WorkerMedium {
    tx: SyncSender<Job>,
    tier: SyncTier,
    health: Health,
}

impl WorkerMedium {
    fn request(&mut self, op: MediumOp) -> Result<WorkerReply, MediumError> {
        // THE TERMINAL-POISON ADMISSION CONTRACT (C2-F2): a poisoned (or
        // exited) worker refuses every NEW operation at the handle itself —
        // existing proxy handles included. Nothing is admitted, so this is
        // a definite non-event, never the uncertain Unknown class.
        if !self.health.alive() {
            return Err(MediumError::WorkerUnavailable {
                detail: "the I/O worker is unavailable (poisoned or exited) — admission refused"
                    .to_owned(),
            });
        }
        let (reply, slot) = completion_slot();
        if self
            .tx
            .send(Job {
                op,
                cancel: None,
                reply,
            })
            .is_err()
        {
            return Err(MediumError::WorkerUnavailable {
                detail: "the I/O worker's queue is closed (the worker exited)".to_owned(),
            });
        }
        slot.recv()
            .map_err(|RecvError| MediumError::WorkerUnavailable {
                detail: "the I/O worker died before replying".to_owned(),
            })?
    }
}

impl CanonicalMedium for WorkerMedium {
    fn head(&mut self) -> Result<Option<Hash>, MediumError> {
        match self.request(MediumOp::Head)? {
            WorkerReply::Head(head) => Ok(head),
            other => unreachable!("the head request returned {other:?}"),
        }
    }

    fn append(
        &mut self,
        record: &RevisionRecord,
        objects: &[StoredObject],
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        // The same terminal-poison admission gate (C2-F2): a definite
        // refusal BEFORE anything is admitted — the job never started, so
        // this is never the uncertain Unknown class.
        if !self.health.alive() {
            return Err(MediumError::WorkerUnavailable {
                detail: "the I/O worker is unavailable (poisoned or exited) — admission refused"
                    .to_owned(),
            });
        }
        let op = MediumOp::Append {
            record: record.clone(),
            objects: objects.to_vec(),
            expected_parent,
        };
        let (reply, slot) = completion_slot();
        if self
            .tx
            .send(Job {
                op,
                cancel: None,
                reply,
            })
            .is_err()
        {
            // Admission failed: the queue's receiver is gone — THIS job
            // never started (definite for it; the session is dead).
            return Err(MediumError::WorkerUnavailable {
                detail: "the I/O worker's queue is closed (the worker exited)".to_owned(),
            });
        }
        match slot.recv() {
            Ok(result) => match result {
                Ok(WorkerReply::Append(outcome)) => Ok(outcome),
                Ok(other) => unreachable!("the append request returned {other:?}"),
                Err(error) => Err(error),
            },
            // THE UNCERTAIN CLASS: the job was admitted; the worker died (or
            // its reply was lost) after the append MAY have executed. Never
            // a definite failure — the candidate's identity is preserved
            // upstream by the pipeline's retained pending attempt.
            Err(_) => Ok(AppendOutcome::Unknown {
                reason: UnknownReason::WorkerReplyLost,
            }),
        }
    }

    fn read_record(&mut self, id: &Hash) -> Result<RevisionRecord, MediumError> {
        match self.request(MediumOp::ReadRecord { id: *id })? {
            WorkerReply::ReadRecord(record) => Ok(record),
            other => unreachable!("the read-record request returned {other:?}"),
        }
    }

    fn read_object(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
        match self.request(MediumOp::ReadObject { hash: *hash })? {
            WorkerReply::ReadObject(bytes) => Ok(bytes),
            other => unreachable!("the read-object request returned {other:?}"),
        }
    }

    fn validate(&mut self) -> Result<ChainSummary, MediumError> {
        match self.request(MediumOp::Validate)? {
            WorkerReply::Validate(summary) => Ok(summary),
            other => unreachable!("the validate request returned {other:?}"),
        }
    }

    fn reconcile_unknown(&mut self, candidate: &Hash) -> Result<Reconciliation, MediumError> {
        match self.request(MediumOp::ReconcileUnknown {
            candidate: *candidate,
        })? {
            WorkerReply::Reconcile(reconciliation) => Ok(reconciliation),
            other => unreachable!("the reconcile request returned {other:?}"),
        }
    }

    fn durability_tier(&self) -> SyncTier {
        self.tier
    }

    fn reclaim(&mut self, units: &[ReclaimUnit]) -> Result<u64, MediumError> {
        match self.request(MediumOp::Reclaim {
            units: units.to_vec(),
        })? {
            WorkerReply::Reclaim(count) => Ok(count),
            other => unreachable!("the reclaim request returned {other:?}"),
        }
    }

    fn reclaim_pending(&mut self, units: &[ReclaimUnit]) -> Result<bool, MediumError> {
        match self.request(MediumOp::ReclaimPending {
            units: units.to_vec(),
        })? {
            WorkerReply::ReclaimPending(pending) => Ok(pending),
            other => unreachable!("the reclaim-pending request returned {other:?}"),
        }
    }

    /// The lifecycle boundary is EXPLICITLY routed through the worker
    /// (never the trait's default no-op): the worker's provider applies
    /// its own policy — the ratified WAL `TRUNCATE` for SQLite, the
    /// no-op for the file medium (whose publications already fsync).
    fn checkpoint(&mut self) -> Result<(), MediumError> {
        match self.request(MediumOp::Checkpoint)? {
            WorkerReply::Checkpoint => Ok(()),
            other => unreachable!("the checkpoint request returned {other:?}"),
        }
    }
}

// ----------------------------------------------------------- the source

/// The worker-backed [`MediumSource`] (PP-B4 checkpoint 2): cloneable;
/// every handle it opens is a queue proxy — never an independent
/// provider connection. `alive` reports the shared health cell, so a
/// dead or poisoned worker fails every authoritative crossing
/// operation closed through the source gate.
#[derive(Clone)]
pub struct WorkerSource {
    tx: SyncSender<Job>,
    tier: SyncTier,
    health: Health,
}

impl MediumSource for WorkerSource {
    type Medium = WorkerMedium;

    fn open_medium(&self) -> Result<WorkerMedium, MediumError> {
        Ok(WorkerMedium {
            tx: self.tx.clone(),
            tier: self.tier,
            health: self.health.clone(),
        })
    }

    fn alive(&self) -> bool {
        self.health.alive()
    }
}

// ------------------------------------------------------------- the handle

/// The campaign's single canonical I/O worker: the handle owns the
/// thread's join, its queue admission, its health cell, and the test
/// fault control. Spawn opens the ONE provider connection (moved into
/// the thread) and reads the truthful [`SyncTier`] from it.
pub struct IoWorker {
    tx: Option<SyncSender<Job>>,
    health: Health,
    tier: SyncTier,
    fault: WorkerFaultControl,
    handle: Option<JoinHandle<()>>,
}

impl IoWorker {
    /// Spawns the worker over the campaign location with the
    /// provisional default queue capacity.
    pub fn spawn(location: &StoreLocation) -> Result<Self, MediumError> {
        Self::spawn_with_faults(location, DEFAULT_QUEUE_CAPACITY, WorkerFaultControl::new())
    }

    /// The fault-controlled spawn (test-only surface).
    #[doc(hidden)]
    pub fn spawn_with_faults(
        location: &StoreLocation,
        capacity: usize,
        fault: WorkerFaultControl,
    ) -> Result<Self, MediumError> {
        // The ONE provider connection — opened here, moved into the thread;
        // the proxies never open another.
        let store = SessionStore::open(location)?;
        let tier = store.durability_tier();
        let (tx, rx) = sync_channel(capacity);
        let cell = Arc::new(AtomicU8::new(HEALTH_ALIVE));
        let handle = thread::Builder::new()
            .name("canonical-io-worker".to_owned())
            .spawn({
                let fault = fault.clone();
                let health = Health(cell.clone());
                move || worker_loop(rx, store, health, fault)
            })
            .map_err(|error| MediumError::Io {
                op: "io-worker spawn",
                path: location.display_path(),
                source: error,
            })?;
        Ok(IoWorker {
            tx: Some(tx),
            health: Health(cell),
            tier,
            fault,
            handle: Some(handle),
        })
    }

    /// The worker-backed source (cloneable; each `open_medium` is a
    /// queue proxy to the one serialization point).
    pub fn source(&self) -> Result<WorkerSource, MediumError> {
        match &self.tx {
            Some(tx) => Ok(WorkerSource {
                tx: tx.clone(),
                tier: self.tier,
                health: self.health.clone(),
            }),
            None => Err(MediumError::WorkerUnavailable {
                detail: "the I/O worker is stopped".to_owned(),
            }),
        }
    }

    /// Whether the worker thread is alive — readable WITHOUT a
    /// blocking worker request (the shared health cell; any exit
    /// path, including a panic, flips it through the drop guard).
    pub fn healthy(&self) -> bool {
        self.health.alive()
    }

    /// The cached durability tier, read from the actual provider at
    /// spawn (truthful; never a round trip).
    pub fn tier(&self) -> SyncTier {
        self.tier
    }

    /// POISON the session: mark the worker unusable for authoritative
    /// progression — every crossing operation refuses through the
    /// source's `alive` gate, journal-only deactivations included.
    /// Idempotent; the thread itself is unaffected (it drains
    /// harmlessly). Worker deaths flip the same cell automatically.
    pub fn poison(&self) {
        self.health.dead();
    }

    /// STOP: close this handle's admission (drops its queue sender).
    /// The worker exits once every other sender — the sources and
    /// proxies — is dropped; `join` waits for that exit.
    pub fn stop(&mut self) {
        self.tx = None;
    }

    /// JOIN: stop, then wait for the thread's exit (all sources and
    /// proxies must already be dropped). A panicked worker surfaces as
    /// the typed unavailable error.
    pub fn join(mut self) -> Result<(), MediumError> {
        self.stop();
        match self.handle.take() {
            Some(handle) => handle.join().map_err(|_| MediumError::WorkerUnavailable {
                detail: "the I/O worker thread panicked".to_owned(),
            }),
            None => Ok(()),
        }
    }

    /// Test-only: submit one raw job and receive its reply receiver
    /// (the abandoned-receiver and queue-saturation evidence; also the
    /// fixture's foreign-writer path).
    #[doc(hidden)]
    pub fn submit_probe(
        &self,
        op: MediumOp,
    ) -> Result<Receiver<Result<WorkerReply, MediumError>>, MediumError> {
        let (reply, slot) = completion_slot();
        match &self.tx {
            Some(tx) => tx
                .send(Job {
                    op,
                    cancel: None,
                    reply,
                })
                .map(|()| slot)
                .map_err(|_| MediumError::WorkerUnavailable {
                    detail: "the I/O worker's queue is closed (the worker exited)".to_owned(),
                }),
            None => Err(MediumError::WorkerUnavailable {
                detail: "the I/O worker is stopped".to_owned(),
            }),
        }
    }

    /// The OPTIONAL-operation admission (PP-B4 checkpoint 3): a
    /// health-checked NONBLOCKING try-send — a full or unavailable
    /// queue returns the typed refusal WITHOUT any canonical action
    /// (nothing was admitted, nothing appended). The cancellation
    /// token is checked once by the worker before execution starts.
    pub fn try_submit(
        &self,
        op: MediumOp,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<Receiver<Result<WorkerReply, MediumError>>, TrySubmitError> {
        if !self.healthy() {
            return Err(TrySubmitError::Unavailable);
        }
        let (reply, slot) = completion_slot();
        let job = Job { op, cancel, reply };
        match &self.tx {
            Some(tx) => match tx.try_send(job) {
                Ok(()) => Ok(slot),
                Err(TrySendError::Full(_)) => Err(TrySubmitError::QueueFull),
                Err(TrySendError::Disconnected(_)) => Err(TrySubmitError::Unavailable),
            },
            None => Err(TrySubmitError::Unavailable),
        }
    }

    /// The REQUIRED-operation admission (PP-B4 checkpoint 4): a
    /// health-checked BLOCKING send — required work WAITS for queue
    /// room (the worker keeps draining, so admission terminates) and
    /// is never dropped, unlike the optional `try_submit`. The
    /// maintenance FIFO barrier and the graceful close's final
    /// snapshot publication ride this path; a full queue is waited
    /// out, an unavailable worker the typed refusal. The job is
    /// never cancelable (no token).
    pub fn submit_required(
        &self,
        op: MediumOp,
    ) -> Result<Receiver<Result<WorkerReply, MediumError>>, MediumError> {
        if !self.healthy() {
            return Err(MediumError::WorkerUnavailable {
                detail: "the I/O worker is unavailable (poisoned or exited) — required \
                         admission refused"
                    .to_owned(),
            });
        }
        let (reply, slot) = completion_slot();
        let job = Job {
            op,
            cancel: None,
            reply,
        };
        match &self.tx {
            Some(tx) => tx
                .send(job)
                .map(|()| slot)
                .map_err(|_| MediumError::WorkerUnavailable {
                    detail: "the I/O worker's queue is closed (the worker exited)".to_owned(),
                }),
            None => Err(MediumError::WorkerUnavailable {
                detail: "the I/O worker is stopped".to_owned(),
            }),
        }
    }

    /// Test-only: the per-instance fault control.
    #[doc(hidden)]
    pub fn fault_control(&self) -> WorkerFaultControl {
        self.fault.clone()
    }
}
