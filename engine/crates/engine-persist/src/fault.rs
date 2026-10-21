//! The crash-window fault-injection seam — **test infrastructure**
//! required by the Batch 2 obligations (the matrix kills or fails a
//! real child process at every storage boundary). A medium constructed
//! without a [`FaultControl`] never consults this module; production
//! paths carry nothing beyond an `Option` check per boundary.
//!
//! `Abort` terminates the process abruptly at the boundary
//! (`std::process::abort` — no unwinding, no cleanup, the OS releases
//! any held file lock), which is how the real-process crash matrix
//! exercises genuine termination rather than simulated exceptions.

use std::collections::BTreeMap;
use std::io;

use crate::medium::MediumError;

/// The storage boundaries the crash-window matrix injects at. Each
/// maps to a precise point in the nine-step append or the
/// reconciliation path (see the Batch 2 storage record's matrix
/// section for the exact placements and expected outcomes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultPoint {
    /// Before `HEAD.lock` acquisition (step 1).
    BeforeLock,
    /// After the lock is held.
    AfterLock,
    /// After the observed HEAD was validated (step 2).
    AfterValidation,
    /// After the expected-parent comparison passed (steps 3–4).
    AfterComparison,
    /// Mid object creation: the first object's bytes are written, not
    /// yet synced, further objects pending (step 5).
    MidObjectWrite,
    /// All object bytes written, none synced yet (step 5).
    ObjectsWritten,
    /// All object files synced, before the objects-directory sync
    /// (step 5).
    ObjectsSynced,
    /// After the objects-directory sync, before HEAD.tmp (step 5 end).
    AfterObjectsDirSync,
    /// HEAD.tmp written, before its sync (step 6).
    HeadTmpWritten,
    /// HEAD.tmp synced, immediately before the rename — the protocol's
    /// linearization point (step 6).
    HeadTmpSynced,
    /// Immediately after the rename (step 6 completed).
    AfterHeadRename,
    /// After the post-rename root-directory sync, before the
    /// acknowledgment returns (step 7 end).
    AfterRootDirSync,
    /// Inside reconciliation, after re-acquiring the lock (step 9).
    ReconcileLocked,
    /// Inside reconciliation, after reading HEAD, before classifying
    /// (step 9).
    ReconcileHeadRead,
}

impl FaultPoint {
    /// The stable name used by the process-test harness environment.
    pub fn name(&self) -> &'static str {
        match self {
            FaultPoint::BeforeLock => "before-lock",
            FaultPoint::AfterLock => "after-lock",
            FaultPoint::AfterValidation => "after-validation",
            FaultPoint::AfterComparison => "after-comparison",
            FaultPoint::MidObjectWrite => "mid-object-write",
            FaultPoint::ObjectsWritten => "objects-written",
            FaultPoint::ObjectsSynced => "objects-synced",
            FaultPoint::AfterObjectsDirSync => "after-objects-dir-sync",
            FaultPoint::HeadTmpWritten => "head-tmp-written",
            FaultPoint::HeadTmpSynced => "head-tmp-synced",
            FaultPoint::AfterHeadRename => "after-head-rename",
            FaultPoint::AfterRootDirSync => "after-root-dir-sync",
            FaultPoint::ReconcileLocked => "reconcile-locked",
            FaultPoint::ReconcileHeadRead => "reconcile-head-read",
        }
    }

    /// Parses the harness name form.
    pub fn from_name(name: &str) -> Option<Self> {
        const POINTS: &[FaultPoint] = &[
            FaultPoint::BeforeLock,
            FaultPoint::AfterLock,
            FaultPoint::AfterValidation,
            FaultPoint::AfterComparison,
            FaultPoint::MidObjectWrite,
            FaultPoint::ObjectsWritten,
            FaultPoint::ObjectsSynced,
            FaultPoint::AfterObjectsDirSync,
            FaultPoint::HeadTmpWritten,
            FaultPoint::HeadTmpSynced,
            FaultPoint::AfterHeadRename,
            FaultPoint::AfterRootDirSync,
            FaultPoint::ReconcileLocked,
            FaultPoint::ReconcileHeadRead,
        ];
        POINTS.iter().copied().find(|point| point.name() == name)
    }
}

/// What the seam does when it fires at an armed point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAction {
    /// The process aborts abruptly at the boundary — a real crash at a
    /// real storage boundary (the OS releases the lock on death).
    Abort,
    /// The boundary's operation returns an injected I/O failure; the
    /// medium must classify it honestly (`Unknown` inside the
    /// uncertain window).
    InjectError,
}

/// An armed map of point → action. Test-only; constructed via
/// [`LocalFileMedium::open_with`](crate::LocalFileMedium::open_with).
#[derive(Debug, Clone, Default)]
pub struct FaultControl {
    armed: BTreeMap<FaultPoint, FaultAction>,
}

impl FaultControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arms a point (builder form).
    pub fn arm(mut self, point: FaultPoint, action: FaultAction) -> Self {
        self.armed.insert(point, action);
        self
    }

    /// Fires the armed action for `point`, if any.
    pub(crate) fn check(&self, point: FaultPoint) -> Result<(), MediumError> {
        match self.armed.get(&point) {
            None => Ok(()),
            Some(FaultAction::Abort) => std::process::abort(),
            Some(FaultAction::InjectError) => Err(MediumError::Io {
                op: "fault-injected",
                path: point.name().to_owned(),
                source: io::Error::other("fault-injected"),
            }),
        }
    }
}
