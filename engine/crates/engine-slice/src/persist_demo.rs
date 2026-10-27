//! Slice 0 integration demonstration (`--demo-persist`): ONE
//! authoritative, persistently-orchestrated campaign behind the
//! existing Vulkan render loop. This module composes — never
//! reimplements — the accepted mechanisms: the SDL3 shell and camera,
//! the Vulkan renderer, the fixed-step simulation discipline of
//! [`crate::sim::SimDriver`] (mirrored here because the authoritative
//! state now lives inside [`OrchestratedSession`], which owns the
//! spine and director), the RegionDirector presence semantics, the
//! PP-B1 SQLite store location, and the PP-B4 orchestrated session
//! (the worker-backed crossing, the optional snapshot publications,
//! the required canonical confirmations, and the graceful durable
//! close).
//!
//! **The integration contract (the GO's five behaviors):**
//!
//! *A — start or recover a campaign.* The store is the explicit
//! `StoreLocation::Database` form (the owner-ratified SQLite
//! provider); a first launch founds and immediately publishes a
//! recovery anchor; a relaunch recovers from the durable state alone.
//! Corrupt, incompatible, divergent, or unverifiable state fails
//! closed with the typed diagnostic — this module never overwrites,
//! never founds over an existing campaign, and never invents a
//! replacement.
//!
//! *B — one authoritative simulation.* There is exactly one
//! [`OrchestratedSession`]; the render loop derives everything
//! read-only through [`PersistDemo::session`] (the same downstream
//! discipline the pipeline-free render demo applies to its own
//! driver). The app owns only the fixed-step timing: the accumulator
//! and bounded catch-up mirror `SimDriver::advance` exactly —
//! wall-clock time never enters logical state, and outstanding
//! obligations are retained and reported, never discarded. The time
//! accounting is ABSOLUTE across sessions: the recovered starting
//! clock is included, so `ticks_due_total == campaign clock +
//! outstanding` holds for founded, recovered, recovered-with-
//! outstanding, and stalled-frame runs alike.
//!
//! *C — one deliberate player action.* `Space` starts a bounded
//! PRESENCE EXCURSION on the camera's region: the region the player
//! stands in is driven through the accepted PP-B4 consequence
//! lifecycle — the durable deactivation (its NPCs' accrued uncommitted
//! consequential signals captured under the T1 journal before the
//! discard), the required canonical confirmation (the `Confirmed`
//! receipt, attributed to the TARGET's actual consequence identities
//! only — unrelated same-ticket regional captures are never claimed),
//! and the reactivation (the captures applied as exactly-once
//! credits, verified against the action's actual captured delta
//! BEFORE completion is reported). An action whose target does not
//! reactivate stays honestly `AwaitingReactivation` — durably
//! confirmed, credits not yet applied, never claimed complete. An
//! empty-capture excursion completes honestly as a zero-consequence
//! no-op. One action at a time: a press during an active or unsettled
//! excursion is refused without changing the active action's target
//! or attribution.
//!
//! *D — truthful persistence state.* The overlay reports the actual
//! lifecycle: the action's phase, the capture counts, the tracked
//! publication (pending-unverified / `Accepted` with the provider
//! tier / stale / unknown / failed / canceled — each its own label),
//! the required-confirmation outcomes, and the absolute time
//! accounting. Nothing is ever called "saved" without a verified
//! receipt.
//!
//! *E — quit and restart.* EVERY exit path after the campaign opened
//! — normal and application-failed alike — runs the single
//! [`PersistDemo::finalize`] site: the accepted graceful close (the
//! final required snapshot with readback verification, the SQLite WAL
//! TRUNCATE lifecycle checkpoint, the settled required confirmations
//! — `failure == None` is the only complete success), with the
//! original application error preserved and the close outcome
//! reported independently; a failed close is never represented as
//! successful. A relaunch recovers exactly — no input replay, no
//! duplicated consequence.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine_core::{LodLevel, PresenceSample, Spine, Tick};
use engine_persist::{
    AuthorityMode, CanonicalMedium, CommitOutcome, ConsequenceId, Hash, RevisionRecord, SyncTier,
    T1Journal, UnknownReason,
};
use engine_renderer::{PresentModePreference, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{
    DriverPreference, InputSnapshot, Keycode, ShellApp, ShellConfig, resident_set_kib,
};

use crate::crossing::TransitionError;
use crate::frame_timing::FrameTiming;
use crate::math;
use crate::npc::{NPC_COUNT, NpcConfig, NpcWorld, REGION_COUNT, install_initial_schedules};
use crate::orchestration::{
    CloseReport, OrchestratedSession, PublicationHandle, PublicationOutcome, SessionPhase,
    SubmitRefusal,
};
use crate::region::{RegionConfig, RegionDirector, stage4_state_fingerprint_hex};
use crate::region_render::{build_region_scene, camera_presence, levels_compact};
use crate::sim::SimConfig;
use crate::soak_evidence::{
    EvidenceCollector, EvidenceConfig, EvidenceOutcome, EvidenceRecord, FrameSample, MemorySample,
    PersistenceKind, PersistenceSample, RegionSample, TickSample, lod_ordinal,
};
use crate::store::{SessionStore, StoreLocation};

/// The demonstration campaign's identity (the founding record's
/// campaign id — one campaign per directory).
pub const CAMPAIGN_ID: &str = "slice0-integration-demo";

/// The demonstration's fixed initial presence: the camera start
/// position mapped onto the region plane (region 0's plate), so a
/// fresh campaign opens with region 0 Full and the far side of the
/// ring demoting exactly as the graphical run's first frames do —
/// and the headless path, the twin comparison, and the graphical
/// path all begin from the same deterministic input.
pub const DEMO_PRESENCE: PresenceSample = PresenceSample {
    position: (100.0, 9.0),
    velocity: (0.0, 0.0),
};

/// The deliberate action's excursion length (ticks): the region is
/// held at its antipodal presence for exactly this many tick
/// boundaries — long enough for the durable deactivation, the T1
/// capture, and the required confirmation to complete — before the
/// live presence (the camera) resumes.
pub const EXCURSION_TICKS: u64 = 3;

/// Where the demonstration campaign's durable state lives: the SQLite
/// database (the canonical chain) and the T1 journal beside it.
#[derive(Debug, Clone)]
pub struct CampaignPaths {
    pub database: PathBuf,
    pub journal: PathBuf,
}

impl CampaignPaths {
    pub fn new(campaign_dir: &Path) -> Self {
        CampaignPaths {
            database: campaign_dir.join("campaign.db"),
            journal: campaign_dir.join("journal.t1"),
        }
    }
}

/// Demo options; CLI parsing lives in the binary.
#[derive(Debug, Clone)]
pub struct DemoOptions {
    /// The campaign directory (created if absent; the database and
    /// journal live inside it — outside the tracked repository).
    pub campaign_dir: PathBuf,
    /// CPU pacing target for the graphical loop. 0 = uncapped.
    pub fps: u32,
    /// Auto-quit after N presented frames (smoke runs).
    pub frames: Option<u64>,
    pub overlay: bool,
    pub validation: ValidationPreference,
    pub present: PresentModePreference,
    /// Run the demonstration headlessly (no GPU, no window): the same
    /// campaign lifecycle, tick discipline, action, publication, and
    /// close, driven by the fixed [`DEMO_PRESENCE`] — the
    /// development-workspace and CI evidence path (graphical
    /// verification belongs to physical hardware).
    pub headless: bool,
    /// Headless mode: the exact number of fixed ticks to run.
    pub headless_ticks: u64,
    /// Headless mode: the tick index at which the deliberate action
    /// fires (None = no action this run).
    pub action_at: Option<u64>,
    /// Test-only (the HOLD-1 regression's controlled seam): poison the
    /// persistence worker at this tick index — the next tick refuses
    /// through the composed gate, exercising the application error
    /// path's finalization deterministically. Never set by the binary.
    #[doc(hidden)]
    pub poison_at_tick: Option<u64>,
    /// S6-C1 observational evidence collection (opt-in; `None` is the
    /// default — zero cost, zero behavior change): when set, the run
    /// carries a [`EvidenceCollector`] whose bounded non-blocking
    /// streams record the full-run measurements, finalized through
    /// the single finalization site AFTER the graceful close — a
    /// failed evidence package never bypasses it.
    pub evidence: Option<EvidenceConfig>,
    /// Test-only (the S6-C1 error-path regression's controlled seam):
    /// arm the evidence writer's fail-writes fault before the run so
    /// the writer fails deterministically mid-campaign. Never set by
    /// the binary.
    #[doc(hidden)]
    pub evidence_fail_writes: bool,
}

/// One advance step's outcome (observational, mirrors `SimStep`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoStep {
    pub executed_ticks: u32,
    /// Due ticks accrued but not yet executed — a retained obligation
    /// that drains when catch-up capacity allows (never discarded).
    pub outstanding_ticks: u32,
    pub campaign_tick: Tick,
}

/// The region whose plate the presence stands on (the deliberate
/// action's target — deterministic given the config and presence).
pub fn nearest_region(config: &RegionConfig, presence: &PresenceSample) -> usize {
    let mut best = (0usize, f64::INFINITY);
    for (region, &(cx, cy)) in config.centers.iter().enumerate() {
        let dx = cx - presence.position.0;
        let dy = cy - presence.position.1;
        let distance = dx * dx + dy * dy;
        if distance < best.1 {
            best = (region, distance);
        }
    }
    best.0
}

/// The deliberate action's excursion presence: the target region's
/// antipode — maximally far from that region (a guaranteed Dormant
/// target through the accepted LOD thresholds) while standing on the
/// opposite region's plate.
pub fn region_antipode(config: &RegionConfig, region: usize) -> PresenceSample {
    let (cx, cy) = config.centers[region];
    PresenceSample {
        position: (-cx, -cy),
        velocity: (0.0, 0.0),
    }
}

/// The region's NPCs' uncommitted consequential signals (the scratch
/// the excursion's deactivation will capture).
pub fn region_signal_sum(world: &NpcWorld, region: usize) -> u64 {
    (0..NPC_COUNT)
        .filter(|npc| npc % REGION_COUNT == region)
        .map(|npc| world.pending_signals()[npc])
        .sum()
}

/// The region's NPCs' committed consequential credits (the durable
/// state the reactivation's exactly-once application grows).
pub fn region_credit_sum(world: &NpcWorld, region: usize) -> u64 {
    (0..NPC_COUNT)
        .filter(|npc| npc % REGION_COUNT == region)
        .map(|npc| world.credits()[npc])
        .sum()
}

/// The deliberate action's attribution state (HOLD-3: the honest,
/// target-attributed lifecycle — an unrelated regional confirmation is
/// never claimed, and completed credit application is reported only
/// after the target actually reactivated and the applied change
/// matched the captured delta).
#[derive(Debug, Clone)]
pub enum ActionPhase {
    /// The excursion is running (the target region recorded).
    Excursion,
    /// The target's deactivation captured nothing (an empty-scratch
    /// deactivation journals nothing, by the accepted rule) — the
    /// action completed honestly as a zero-consequence no-op. A
    /// same-ticket receipt for OTHER regions' captures is never
    /// attributed.
    ZeroCapture,
    /// The target's capture is durably confirmed (the receipt's
    /// TARGET-REGION identities only); the target has not yet
    /// reactivated — the credits are NOT yet applied. An honest
    /// pending state, never claimed complete; it completes at the
    /// target's next reactivation.
    AwaitingReactivation { keys: usize, revision: Hash },
    /// The target reactivated and the captured delta was applied as
    /// credits — the applied change verified against the action's
    /// actual captured delta.
    Applied { keys: usize, revision: Hash },
    /// Attribution could not be verified honestly (an unknown or
    /// stale confirmation, or an applied credit change that did not
    /// match the captured delta) — incomplete, never claimed
    /// complete.
    Incomplete { reason: String },
}

impl ActionPhase {
    /// Whether a subsequent deliberate action may begin: only the
    /// settled states (an active excursion or an unreactivated
    /// capture refuses a new press).
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self,
            ActionPhase::Excursion | ActionPhase::AwaitingReactivation { .. }
        )
    }
}

/// One deliberate action's full record: the target region, the
/// excursion window, the captured-delta expectation, and the honest
/// attribution state.
#[derive(Debug, Clone)]
pub struct ActionRecord {
    pub region: usize,
    /// The tick index the excursion started at (each accepted action's
    /// independent identity for start/completion reporting).
    pub started_at: u64,
    pub excursion_ticks: u64,
    /// The target region's uncommitted signal scratch at the action's
    /// start — the captured-delta expectation the completion verifies
    /// the applied credits against.
    pub captured_delta: u64,
    pub credits_before: u64,
    pub phase: ActionPhase,
    /// `Some` once the action's credit outcome is observed (the
    /// applied sum for a completed application; the unchanged sum for
    /// a zero-capture no-op) — never claimed before it happens.
    pub credits_after: Option<u64>,
}

impl ActionRecord {
    /// The overlay/print form — the truthful progressive states, never
    /// claiming confirmation or application before it is observed.
    pub fn describe(&self) -> String {
        let phase = match &self.phase {
            ActionPhase::Excursion => "excursion running".to_string(),
            ActionPhase::ZeroCapture => {
                "zero-capture no-op (nothing captured, nothing applied)".to_string()
            }
            ActionPhase::AwaitingReactivation { keys, revision } => format!(
                "confirmed {keys} keys @ {revision} — awaiting the target's reactivation (credits NOT yet applied)"
            ),
            ActionPhase::Applied { keys, revision } => {
                format!("applied {keys} keys @ {revision}")
            }
            ActionPhase::Incomplete { reason } => format!("INCOMPLETE — {reason}"),
        };
        let credits_after = self
            .credits_after
            .map(|credits| credits.to_string())
            .unwrap_or_else(|| "…".into());
        format!(
            "excursion r{} @ tick {} ({} ticks) — captured-delta {} · {} · credits {}→{}",
            self.region,
            self.started_at,
            self.excursion_ticks,
            self.captured_delta,
            phase,
            self.credits_before,
            credits_after
        )
    }
}

/// The tracked publication's honest state — the truthful display
/// surface. Never a "saved" claim without the verified `Accepted`
/// receipt; refused, stale, unknown, failed, and canceled are each
/// their own label.
#[derive(Debug, Clone)]
pub enum PublicationState {
    /// Admitted, outcome not yet verified by the application.
    Pending { candidate: Hash },
    /// The provider accepted the revision — the only confirmed state.
    Accepted { revision: Hash, tier: SyncTier },
    /// The stale capture was refused by the CAS — never rebased.
    Stale {
        revision: Hash,
        observed: Option<Hash>,
    },
    /// The disposition could not be verified — never interpreted.
    Unknown {
        revision: Hash,
        reason: UnknownReason,
    },
    /// A definite medium failure — nothing installed.
    Failed { detail: String },
    /// Canceled before the job started — a definite non-event.
    Canceled { revision: Hash },
}

impl PublicationState {
    pub fn is_pending(&self) -> bool {
        matches!(self, PublicationState::Pending { .. })
    }

    /// The truthful overlay label (the word "ACCEPTED" appears only
    /// for the verified receipt; every other class names itself).
    pub fn label(&self) -> String {
        match self {
            PublicationState::Pending { candidate } => {
                format!("pending (unverified) content {candidate}")
            }
            PublicationState::Accepted { revision, tier } => {
                format!("ACCEPTED {revision} ({tier:?})")
            }
            PublicationState::Stale { revision, .. } => {
                format!("STALE — refused, never rebased {revision}")
            }
            PublicationState::Unknown { revision, .. } => {
                format!("UNKNOWN — unverifiable, never confirmed {revision}")
            }
            PublicationState::Failed { detail } => format!("FAILED — {detail}"),
            PublicationState::Canceled { revision } => format!("canceled {revision}"),
        }
    }
}

/// The per-frame truthful persistence status (the overlay's input).
#[derive(Debug, Clone)]
pub struct PersistenceStatus {
    pub founded: bool,
    pub tick: Tick,
    /// False under maintenance or closing (authoritative progression
    /// suspended — displayed, never hidden).
    pub phase_active: bool,
    pub worker_healthy: bool,
    pub tier: SyncTier,
    pub pending_publications: usize,
    pub publication: Option<PublicationState>,
    /// The tracked publication's capture tick — the honest pending
    /// provenance (when the unverified state was captured).
    pub publication_capture_tick: Option<u64>,
    /// The last nonblocking-admission refusal (Busy/Stale/…), if any.
    pub last_refusal: Option<SubmitRefusal>,
    /// The last capture failure, if any (a typed surface, displayed).
    pub last_capture_error: Option<String>,
    /// T1-captured consequential deltas awaiting canonical
    /// confirmation (the director ledger's pending lane).
    pub captures_pending: u64,
    /// Applied captures — committed exactly once at reactivations.
    pub captures_applied: u64,
    /// The last canonically confirmed revision (an `Accepted`
    /// publication or a `Confirmed` required confirmation).
    pub confirmed_revision: Option<Hash>,
    /// The last required-confirmation outcome's label.
    pub last_commit: Option<String>,
    pub action: Option<ActionRecord>,
}

/// The launch report: what this application start did with the
/// campaign (founded + anchored, or recovered — and from what state).
#[derive(Debug, Clone)]
pub struct LaunchReport {
    pub founded: bool,
    pub database: PathBuf,
    pub journal: PathBuf,
    /// A fresh campaign's recovery-anchor revision (published and
    /// verified at open, so the campaign is recoverable from the
    /// moment it starts).
    pub anchor_revision: Option<Hash>,
    /// A recovered campaign's restored clock (the absolute time
    /// accounting's starting offset).
    pub recovered_at_tick: Option<Tick>,
    /// A recovered campaign's restored stage-4 fingerprint.
    pub recovered_fingerprint: Option<String>,
}

struct TrackedPublication {
    handle: PublicationHandle,
    /// The capture's simulation tick — the pending display's honest
    /// provenance (when the unverified state was captured).
    capture_tick: u64,
    /// The admission instant (S6-C1): the completion-latency
    /// measurement's start — admission-to-observed-completion, the
    /// latency the authority thread actually experiences (queue +
    /// append + fsync + readback), never the worker's interior.
    submitted_at: Instant,
    state: PublicationState,
}

/// The deliberate action's in-progress excursion state.
struct ExcursionState {
    region: usize,
    remaining: u64,
}

/// The application-level finalization outcome (HOLD-1): the preserved
/// application failure (None on normal exit), the graceful close's
/// honest report, and (when evidence collection was enabled) the
/// evidence package's own verdict — combined so the original error
/// stays visible, the close outcome is reported independently, a
/// failed close is never represented as successful, and an incomplete
/// evidence package is a failed run without ever having bypassed the
/// close.
pub struct Finalization {
    pub application_failure: Option<String>,
    pub close: CloseReport,
    /// The evidence collector's verdict (S6-C1) — `None` when the run
    /// collected no evidence (the default).
    pub evidence: Option<EvidenceOutcome>,
}

impl Finalization {
    /// The combined outcome: `Err` iff anything failed — the original
    /// application error first, the close outcome stated
    /// independently, the evidence verdict last; never a success
    /// claim over a failed close or a lost-evidence run.
    pub fn combined(&self) -> Result<(), String> {
        let close_failure = self
            .close
            .failure
            .as_ref()
            .map(|failure| format!("{failure:?}"));
        let base = match (&self.application_failure, close_failure) {
            (None, None) => Ok(()),
            (Some(application), None) => Err(format!(
                "{application}; the graceful close COMPLETED (final {}, checkpoint {}, settled {})",
                self.close
                    .final_revision
                    .map(|revision| revision.to_string())
                    .unwrap_or_else(|| "none".into()),
                self.close.lifecycle_checkpoint,
                self.close.required_settled
            )),
            (None, Some(close)) => Err(format!(
                "the close did not complete: {close} — the campaign remains recoverable from its durable state"
            )),
            (Some(application), Some(close)) => Err(format!(
                "{application}; AND the close did not complete: {close} — the verified evidence stands; \
                 the campaign remains recoverable from its durable state"
            )),
        };
        let evidence_failure = self
            .evidence
            .as_ref()
            .filter(|outcome| !outcome.is_complete())
            .map(|outcome| {
                format!(
                    "the evidence package is incomplete ({} samples, first failure: {})",
                    outcome.total_samples(),
                    outcome.failure.as_deref().unwrap_or("counts mismatched")
                )
            });
        match (base, evidence_failure) {
            (Ok(()), None) => Ok(()),
            (Ok(()), Some(evidence)) => Err(format!(
                "{evidence}; the graceful close COMPLETED independently (final {}, checkpoint {}, settled {})",
                self.close
                    .final_revision
                    .map(|revision| revision.to_string())
                    .unwrap_or_else(|| "none".into()),
                self.close.lifecycle_checkpoint,
                self.close.required_settled
            )),
            (Err(existing), None) => Err(existing),
            (Err(existing), Some(evidence)) => Err(format!("{existing}; AND {evidence}")),
        }
    }

    /// The close's extracted summary (report-safe).
    pub fn close_summary(&self) -> CloseSummary {
        CloseSummary::of(&self.close)
    }
}

/// The graceful close's extracted summary (report-safe: the failure
/// as its display form).
#[derive(Debug, Clone)]
pub struct CloseSummary {
    pub final_revision: Option<Hash>,
    pub final_tier: Option<SyncTier>,
    pub lifecycle_checkpoint: bool,
    pub required_settled: bool,
    pub completions: usize,
    pub failure: Option<String>,
}

impl CloseSummary {
    fn of(report: &CloseReport) -> Self {
        CloseSummary {
            final_revision: report.final_revision,
            final_tier: report.final_tier,
            lifecycle_checkpoint: report.lifecycle_checkpoint,
            required_settled: report.required_settled,
            completions: report.completions.len(),
            failure: report
                .failure
                .as_ref()
                .map(|failure| format!("{failure:?}")),
        }
    }
}

/// The headless run's complete report — the integration tests'
/// assertion surface and the development-workspace evidence.
#[derive(Debug, Clone)]
pub struct HeadlessReport {
    pub launch: LaunchReport,
    /// The preserved application-level failure from the run's loop
    /// (None when the ticks and action ran clean) — HOLD-1: the error
    /// that triggered the finalization, never swallowed.
    pub application_failure: Option<String>,
    pub action: Option<ActionRecord>,
    pub action_publication: Option<PublicationState>,
    pub closed_at_tick: Tick,
    /// The ABSOLUTE campaign ticks due at close (the recovered
    /// starting clock included — HOLD-2's conservation input).
    pub closed_ticks_due: u64,
    /// The retained outstanding catch-up obligations at close.
    pub closed_outstanding: u64,
    /// The stage-4 state fingerprint at close (before the close's own
    /// snapshot — the state the close then durably represents).
    pub closed_fingerprint: String,
    /// The action region's (signals, credits) sums at close — the
    /// durable-outcome witness for the deliberate consequence.
    pub closed_region: Option<(usize, u64, u64)>,
    pub close: CloseSummary,
    /// The evidence package's verdict (S6-C1) — `None` when the run
    /// collected no evidence.
    pub evidence: Option<EvidenceOutcome>,
}

impl HeadlessReport {
    /// The CLI evidence print (also used by the tests' failure
    /// messages).
    pub fn print(&self) {
        println!("Slice 0 integration demonstration — headless report");
        println!(
            "  campaign: {} · db {} · journal {}",
            if self.launch.founded {
                "founded"
            } else {
                "recovered"
            },
            self.launch.database.display(),
            self.launch.journal.display()
        );
        if let Some(anchor) = self.launch.anchor_revision {
            println!("  recovery anchor: {anchor}");
        }
        if let (Some(tick), fingerprint) = (
            self.launch.recovered_at_tick,
            self.launch.recovered_fingerprint.as_deref(),
        ) {
            println!(
                "  recovered at tick {} · restored fingerprint {}",
                tick.as_u64(),
                fingerprint.unwrap_or("?")
            );
        }
        if let Some(failure) = &self.application_failure {
            println!("  APPLICATION FAILURE (preserved): {failure}");
        }
        if let Some(action) = &self.action {
            println!("  action: {}", action.describe());
        }
        if let Some(publication) = &self.action_publication {
            println!("  action publication: {}", publication.label());
        }
        if let Some((region, signals, credits)) = &self.closed_region {
            println!("  closed action region r{region}: signals {signals} · credits {credits}");
        }
        println!(
            "  closed at tick {} · ticks due {} · outstanding {} · conservation {} · stage4 fingerprint {}",
            self.closed_at_tick.as_u64(),
            self.closed_ticks_due,
            self.closed_outstanding,
            self.closed_ticks_due == self.closed_at_tick.as_u64() + self.closed_outstanding,
            self.closed_fingerprint
        );
        println!(
            "  close: final {} · checkpoint {} · required settled {} · failure {:?}",
            self.close
                .final_revision
                .map(|revision| revision.to_string())
                .unwrap_or_else(|| "none".into()),
            self.close.lifecycle_checkpoint,
            self.close.required_settled,
            self.close.failure
        );
        if let Some(outcome) = &self.evidence {
            println!(
                "  evidence: {} samples · verdict {}",
                outcome.total_samples(),
                if outcome.is_complete() {
                    "COMPLETE"
                } else {
                    "INCOMPLETE"
                }
            );
        }
    }
}

/// The required canonical confirmation outcome's truthful label.
fn commit_label(outcome: &CommitOutcome) -> String {
    match outcome {
        CommitOutcome::Confirmed(receipt) => {
            format!(
                "confirmed {} keys @ {}",
                receipt.keys.len(),
                receipt.revision
            )
        }
        CommitOutcome::NothingPending => "nothing pending".to_string(),
        CommitOutcome::Unknown { revision, .. } => {
            format!("UNKNOWN {revision} (re-drive required)")
        }
        CommitOutcome::StaleParent { observed } => {
            format!("stale parent (observed {observed:?})")
        }
    }
}

/// Builds the demonstration's fresh scenario — exactly the
/// `SimDriver::with_regions` construction (the LOD-enabled workload,
/// the reserved spine, the installed routine chains, the director,
/// and the initial presence application) — shared by the session's
/// fresh-open path and the integration tests' never-persisted twin,
/// so both run byte-identical logical work.
pub fn demo_scenario(
    timing: &SimConfig,
    initial_presence: &[PresenceSample],
) -> Result<(Spine<NpcWorld>, RegionDirector), String> {
    let config = NpcConfig {
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let mut spine = Spine::with_reservation(
        NpcWorld::new(config.clone()),
        timing.budget,
        timing.effective_near_reserve(),
    );
    install_initial_schedules(&mut spine, &config);
    let mut director = RegionDirector::new(RegionConfig::default())
        .map_err(|error| format!("the default region configuration: {error:?}"))?;
    director.apply(&mut spine, initial_presence);
    Ok((spine, director))
}

/// The integrated demonstration driver: ONE orchestrated, persisted
/// authoritative session plus the application-owned fixed-step
/// timing (the accumulator and bounded catch-up mirror
/// `SimDriver::advance`; the orchestrated session itself has no
/// accumulator — the caller owns the cadence).
pub struct PersistDemo {
    session: OrchestratedSession,
    timing: SimConfig,
    period_ns: u128,
    /// The campaign clock at this session's start (0 for a fresh
    /// founding; the recovered clock for a relaunch) — the absolute
    /// time-accounting offset (HOLD-2).
    start_tick: u64,
    accumulator_ns: u128,
    /// Total elapsed time accepted via `advance` — timing state only,
    /// never fingerprinted (the same discipline as `SimDriver`).
    total_advanced_ns: u128,
    headless_ticks: u64,
    tick_ms_history: Vec<f64>,
    /// The presence the next tick boundary consumes — explicit
    /// simulation input, never wall-clock (the graphical loop feeds
    /// the camera; the headless path feeds the fixed demo presence).
    presence: Vec<PresenceSample>,
    founded: bool,
    action: Option<ActionRecord>,
    /// The deliberate action's in-progress excursion (the presence
    /// override while active).
    excursion: Option<ExcursionState>,
    publication: Option<TrackedPublication>,
    last_refusal: Option<SubmitRefusal>,
    last_capture_error: Option<String>,
    last_commit: Option<String>,
    confirmed_revision: Option<Hash>,
    /// The S6-C1 evidence collector (opt-in; `None` by default —
    /// every emission site is guarded by it, so an absent collector
    /// is zero cost and zero behavior change).
    evidence: Option<EvidenceCollector>,
}

impl PersistDemo {
    fn empty(session: OrchestratedSession, timing: SimConfig, period_ns: u128) -> Self {
        let start_tick = session.clock().as_u64();
        PersistDemo {
            session,
            timing,
            period_ns,
            start_tick,
            accumulator_ns: 0,
            total_advanced_ns: 0,
            headless_ticks: 0,
            tick_ms_history: Vec::new(),
            presence: vec![DEMO_PRESENCE],
            founded: false,
            action: None,
            excursion: None,
            publication: None,
            last_refusal: None,
            last_capture_error: None,
            last_commit: None,
            confirmed_revision: None,
            evidence: None,
        }
    }

    /// Opens or recovers the campaign at `campaign_dir` (behavior *A*).
    /// An unfounded database founds a fresh campaign and immediately
    /// publishes the verified recovery anchor; a founded database
    /// recovers from the durable state alone. Corrupt, divergent, or
    /// unverifiable state fails closed with the typed diagnostic —
    /// never an overwrite, never a replacement campaign.
    pub fn open_or_recover(
        campaign_dir: &Path,
        timing: SimConfig,
    ) -> Result<(Self, LaunchReport), String> {
        std::fs::create_dir_all(campaign_dir).map_err(|error| {
            format!(
                "create the campaign directory {}: {error}",
                campaign_dir.display()
            )
        })?;
        let paths = CampaignPaths::new(campaign_dir);
        let location = StoreLocation::Database(paths.database.clone());
        let rate = timing.tick_rate_hz.max(1);
        let period_ns = 1_000_000_000u128 / u128::from(rate);
        let mut store = SessionStore::open(&location).map_err(|error| {
            format!(
                "open the campaign store {}: {error} — refusing to touch the durable state",
                location.display_path()
            )
        })?;
        let head = store.head().map_err(|error| {
            format!(
                "read the campaign head {}: {error} — refusing to touch the durable state",
                location.display_path()
            )
        })?;
        if head.is_some() {
            // An existing campaign: recover from the durable state.
            drop(store);
            let session = OrchestratedSession::recover(&location, &paths.journal, CAMPAIGN_ID)
                .map_err(|error| {
                    format!(
                        "recover the campaign at {}: {error:?} — the durable state is untouched",
                        location.display_path()
                    )
                })?;
            let report = LaunchReport {
                founded: false,
                database: paths.database,
                journal: paths.journal,
                anchor_revision: None,
                recovered_at_tick: Some(session.clock()),
                recovered_fingerprint: Some(stage4_state_fingerprint_hex(
                    session.spine(),
                    session.director(),
                )),
            };
            let mut demo = Self::empty(session, timing, period_ns);
            demo.presence = vec![DEMO_PRESENCE];
            Ok((demo, report))
        } else {
            // A fresh campaign: found (the pre-worker discipline), build
            // the scenario, open the orchestrated session, and publish
            // the verified recovery anchor so the campaign is
            // recoverable from the moment it starts.
            store
                .append(
                    &RevisionRecord::founding(AuthorityMode::Standalone, CAMPAIGN_ID, 0),
                    &[],
                    None,
                )
                .map_err(|error| format!("found the campaign: {error}"))?;
            drop(store);
            let presence = vec![DEMO_PRESENCE];
            let (spine, director) = demo_scenario(&timing, &presence)?;
            let journal = T1Journal::open(&paths.journal).map_err(|error| {
                format!(
                    "open the campaign journal {}: {error}",
                    paths.journal.display()
                )
            })?;
            let mut session =
                OrchestratedSession::open(spine, director, &location, journal, CAMPAIGN_ID)
                    .map_err(|error| format!("open the orchestrated session: {error:?}"))?;
            let capture = session
                .capture_snapshot()
                .map_err(|error| format!("capture the recovery anchor: {error:?}"))?;
            let handle = session
                .submit_publication(capture)
                .map_err(|refusal| format!("submit the recovery anchor: {refusal:?}"))?;
            let anchor = match session.wait_publication(handle) {
                Some(PublicationOutcome::Accepted { revision, .. }) => revision,
                other => {
                    return Err(format!(
                        "the recovery anchor did not verify: {other:?} — the campaign is not recoverable; refusing to run"
                    ));
                }
            };
            let report = LaunchReport {
                founded: true,
                database: paths.database,
                journal: paths.journal,
                anchor_revision: Some(anchor),
                recovered_at_tick: None,
                recovered_fingerprint: None,
            };
            let mut demo = Self::empty(session, timing, period_ns);
            demo.founded = true;
            demo.presence = presence;
            Ok((demo, report))
        }
    }

    /// Sets the presence the next tick boundary consumes (explicit
    /// simulation input — the graphical loop feeds the camera here;
    /// the deliberate action's excursion overrides it while active).
    pub fn set_presence(&mut self, presence: Vec<PresenceSample>) {
        self.presence = presence;
    }

    /// The demonstration's fixed-step timing configuration.
    pub fn config(&self) -> SimConfig {
        self.timing
    }

    /// Read-only access to the one authoritative session.
    pub fn session(&self) -> &OrchestratedSession {
        &self.session
    }

    /// Attaches the S6-C1 evidence collector. The loops create it
    /// BEFORE the campaign opens (so recovery is timed), then hand it
    /// here; every emission site afterwards is guarded by it.
    pub fn attach_evidence(&mut self, collector: EvidenceCollector) {
        self.evidence = Some(collector);
    }

    /// The attached evidence collector, when collection is enabled.
    pub fn evidence(&self) -> Option<&EvidenceCollector> {
        self.evidence.as_ref()
    }

    /// The campaign clock at this session's start (the absolute
    /// time-accounting offset — 0 for a fresh founding, the recovered
    /// clock for a relaunch).
    pub fn start_tick(&self) -> u64 {
        self.start_tick
    }

    /// Accrued due-but-unexecuted ticks right now (retained
    /// obligation — reported, never discarded).
    pub fn outstanding_ticks(&self) -> u32 {
        (self.accumulator_ns / self.period_ns) as u32
    }

    /// The session-relative ticks due (this process only — the
    /// advanced-time and headless executions).
    pub fn session_ticks_due(&self) -> u64 {
        (self.total_advanced_ns / self.period_ns) as u64 + self.headless_ticks
    }

    /// The ABSOLUTE campaign ticks due since founding (HOLD-2): the
    /// recovered starting clock plus this session's due ticks — so
    /// `ticks_due_total == campaign clock + outstanding` holds for a
    /// founded campaign, a recovered campaign, a recovered campaign
    /// with retained catch-up obligations, and a long or stalled
    /// graphical frame alike (the no-discard invariant across
    /// sessions, never a false equation).
    pub fn ticks_due_total(&self) -> u64 {
        self.start_tick + self.session_ticks_due()
    }

    /// Advances the authoritative session by elapsed monotonic time —
    /// the `SimDriver::advance` discipline verbatim: elapsed time only
    /// accrues due ticks (bounded per-frame catch-up; the remainder
    /// stays accrued and reported as outstanding).
    pub fn advance(&mut self, elapsed: Duration) -> Result<DemoStep, TransitionError> {
        self.total_advanced_ns += elapsed.as_nanos();
        self.accumulator_ns += elapsed.as_nanos();
        let due = (self.accumulator_ns / self.period_ns) as u32;
        let executed = due.min(self.timing.max_catchup_per_frame);
        self.run_ticks(executed, true)?;
        self.accumulator_ns -= u128::from(executed) * self.period_ns;
        Ok(DemoStep {
            executed_ticks: executed,
            outstanding_ticks: due - executed,
            campaign_tick: self.session.clock(),
        })
    }

    /// Runs exactly `ticks` fixed ticks — the headless deterministic
    /// path (no real-time input at all; these ticks never settle
    /// accrued time debt).
    pub fn run_headless(&mut self, ticks: u64) -> Result<(), TransitionError> {
        self.headless_ticks += ticks;
        self.run_ticks(ticks as u32, false)
    }

    fn run_ticks(&mut self, ticks: u32, settles_debt: bool) -> Result<(), TransitionError> {
        // The fixed-step debt observable at the batch boundary (S6-C1):
        // the accumulator's due count as this batch begins. Per-tick
        // samples report it decrementing as the batch executes; the
        // headless path's free ticks never settle accrued debt.
        let due_now = (self.accumulator_ns / self.period_ns) as u32;
        for index in 0..ticks {
            // The effective presence: the deliberate action's excursion
            // override while active (the region held at its antipode),
            // the caller's presence otherwise. Computed BEFORE the
            // excursion state advances (the tick consumes this
            // presence).
            let antipode = match &self.excursion {
                Some(excursion) if excursion.remaining > 0 => Some(region_antipode(
                    self.session.director().config(),
                    excursion.region,
                )),
                _ => None,
            };
            let during_excursion = antipode.is_some();
            let excursion_target = self.excursion.as_ref().map(|excursion| excursion.region);
            let presence: Vec<PresenceSample> = match antipode {
                Some(sample) => vec![sample],
                None => self.presence.clone(),
            };
            // S6-C1 (observational): the levels before the tick — the
            // transition records' from-side, read only when a
            // collector is attached.
            let levels_before = self
                .evidence
                .as_ref()
                .map(|_| self.session.director().levels());
            // The whole orchestrated boundary — the composed gate, the
            // presence-driven transitions (the durable crossings when
            // they occur), and the spine tick — timed as one unit and
            // reported under its own label, never as bare spine time.
            let start = Instant::now();
            let report = self.session.tick(&presence)?;
            let tick_ms = start.elapsed().as_secs_f64() * 1000.0;
            self.tick_ms_history.push(tick_ms);
            let len = self.tick_ms_history.len();
            if len > 600 {
                self.tick_ms_history.drain(0..len - 600);
            }
            // The excursion state advances AFTER the tick consumed its
            // presence: the far window counts down, and the first
            // post-excursion tick (the live presence again — the
            // reactivation) clears it.
            let clear_excursion = match &mut self.excursion {
                Some(excursion) if excursion.remaining > 0 => {
                    excursion.remaining -= 1;
                    false
                }
                Some(_) => true,
                None => false,
            };
            if clear_excursion {
                self.excursion = None;
            }
            // The target's ACTUAL capture-bearing transition (HOLD-3):
            // the attribution gate — only a deactivation OF THE TARGET
            // during the excursion attributes the confirmation that
            // follows; any other regional transition is background
            // work the action never claims.
            let target_deactivated = during_excursion
                && excursion_target.is_some_and(|region| {
                    report.regions_transitioned.contains(&region)
                        && self.session.region_level(region) == LodLevel::Dormant
                });
            if !report.regions_transitioned.is_empty() {
                // A capture-bearing boundary occurred: drive the
                // required canonical confirmation (the tests'
                // discipline — T1 durability precedes it, and the
                // outcome label lands in the overlay).
                let commit_start = Instant::now();
                let outcome = self.session.commit_canonical()?;
                let confirmation_ms = commit_start.elapsed().as_secs_f64() * 1000.0;
                let label = commit_label(&outcome);
                let revision = if let CommitOutcome::Confirmed(receipt) = &outcome {
                    self.confirmed_revision = Some(receipt.revision);
                    Some(receipt.revision.to_string())
                } else {
                    None
                };
                self.emit_persistence(
                    PersistenceKind::Confirmation,
                    Some(confirmation_ms),
                    label.clone(),
                    revision,
                );
                self.last_commit = Some(label);
                if target_deactivated {
                    self.attribute_action_confirmation(&outcome);
                }
            }
            self.refresh_action();
            self.emit_tick_evidence(
                tick_ms,
                &report,
                levels_before,
                index,
                due_now,
                settles_debt,
            );
        }
        Ok(())
    }

    /// S6-C1 (observational, never on the logical path): the per-tick
    /// evidence — the spine/lane counter snapshots, the tick's
    /// duration, the fixed-step debt, and one region record per
    /// transition with its before/after LOD levels.
    fn emit_tick_evidence(
        &self,
        tick_ms: f64,
        report: &crate::orchestration::TickReport,
        levels_before: Option<[LodLevel; REGION_COUNT]>,
        index: u32,
        due_now: u32,
        settles_debt: bool,
    ) {
        let Some(ev) = self.evidence.as_ref() else {
            return;
        };
        let spine = self.session.spine();
        let stats = spine.stats();
        let totals = spine.lane_totals();
        let outstanding = if settles_debt {
            due_now.saturating_sub(index + 1)
        } else {
            due_now
        };
        let tick = report.tick.as_u64();
        ev.produce(EvidenceRecord::Tick(TickSample {
            tick,
            wall_offset_ms: ev.wall_offset_ms(),
            tick_ms,
            ticks_due_total: self.ticks_due_total(),
            outstanding_ticks: outstanding,
            executed: stats.executed,
            scheduled: stats.scheduled,
            late_arrivals: stats.late_arrivals,
            late_executions: stats.late_executions,
            blocked_transitions: stats.blocked_transitions,
            invalid_dropped: stats.invalid_dropped,
            budget_exhausted_ticks: stats.budget_exhausted_ticks,
            near_pending: spine.near_pending_count() as u32,
            far_pending: spine.far_pending_count() as u32,
            blocked: spine.blocked_count() as u32,
            held: spine.held_count() as u32,
            near_executed_total: totals.near_executed,
            far_executed_total: totals.far_executed,
            near_backlogged_ticks: totals.near_backlogged_ticks,
            far_backlogged_ticks: totals.far_backlogged_ticks,
            processed_through: spine.processed_through().map(|tick| tick.as_u64()),
            max_executed_due: spine.max_executed_due().map(|tick| tick.as_u64()),
            transitioned_regions: report
                .regions_transitioned
                .iter()
                .map(|region| u64::from(*region as u16))
                .collect(),
        }));
        if let Some(before) = levels_before {
            let after = self.session.director().levels();
            for region in &report.regions_transitioned {
                ev.produce(EvidenceRecord::Region(RegionSample {
                    tick,
                    wall_offset_ms: ev.wall_offset_ms(),
                    region: u64::from(*region as u16),
                    from_level: lod_ordinal(before[*region]),
                    to_level: lod_ordinal(after[*region]),
                }));
            }
        }
    }

    /// S6-C1 (observational): one application-boundary persistence
    /// record — the latency families stay separate (capture,
    /// admission, busy, completion, confirmation, close, recovery),
    /// never conflated.
    fn emit_persistence(
        &self,
        kind: PersistenceKind,
        latency_ms: Option<f64>,
        outcome: String,
        revision: Option<String>,
    ) {
        let Some(ev) = self.evidence.as_ref() else {
            return;
        };
        ev.produce(EvidenceRecord::Persistence(PersistenceSample {
            kind,
            wall_offset_ms: ev.wall_offset_ms(),
            tick: self.session.clock().as_u64(),
            latency_ms,
            outcome,
            revision,
        }));
    }

    /// HOLD-3: attribute the action's confirmation to the TARGET's
    /// actual captured identities only. The receipt is filtered
    /// against the target's expected consequence ids (its region,
    /// the deactivation's episode, its NPCs) — unrelated same-ticket
    /// regional captures are never claimed; an empty-capture target
    /// completes honestly as a no-op regardless of the receipt.
    fn attribute_action_confirmation(&mut self, outcome: &CommitOutcome) {
        let Some(action) = &self.action else { return };
        if !matches!(action.phase, ActionPhase::Excursion) {
            return;
        }
        let region = action.region;
        let captured_delta = action.captured_delta;
        if captured_delta == 0 {
            // The target's deactivation captured nothing: journals
            // nothing by the accepted rule — completed as an honest
            // no-op (the observed, unchanged credits); the
            // same-ticket receipt (other regions' captures) is NEVER
            // attributed.
            let credits_after = region_credit_sum(self.session.spine().world(), region);
            if let Some(action) = self.action.as_mut() {
                action.phase = ActionPhase::ZeroCapture;
                action.credits_after = Some(credits_after);
            }
            return;
        }
        let episode = self.session.director().states()[region].episode;
        let expected: HashSet<ConsequenceId> = (0..NPC_COUNT)
            .filter(|npc| npc % REGION_COUNT == region)
            .map(|npc| ConsequenceId::new(region as u32, episode, npc as u32))
            .collect();
        let phase = match outcome {
            CommitOutcome::Confirmed(receipt) => {
                let keys = receipt
                    .keys
                    .iter()
                    .copied()
                    .filter(|key| expected.contains(key))
                    .count();
                if keys > 0 {
                    ActionPhase::AwaitingReactivation {
                        keys,
                        revision: receipt.revision,
                    }
                } else {
                    ActionPhase::Incomplete {
                        reason: "the confirmed batch carried no identities for the target region"
                            .into(),
                    }
                }
            }
            CommitOutcome::Unknown { revision, .. } => ActionPhase::Incomplete {
                reason: format!(
                    "the target's confirmation is UNKNOWN ({revision}) — never interpreted as confirmed"
                ),
            },
            CommitOutcome::StaleParent { observed } => ActionPhase::Incomplete {
                reason: format!("the target's confirmation hit a stale parent ({observed:?})"),
            },
            CommitOutcome::NothingPending => ActionPhase::Incomplete {
                reason: "nothing pending at the target's capture-bearing boundary — \
                         the capture did not journal as expected"
                    .into(),
            },
        };
        if let Some(action) = self.action.as_mut() {
            action.phase = phase;
        }
    }

    /// THE deliberate player action (behavior *C*): starts a bounded
    /// presence excursion on the region the presence stands in — the
    /// region is driven through the accepted consequence lifecycle
    /// (durable deactivation with its NPCs' uncommitted signals
    /// captured under T1, the required canonical confirmation, and
    /// the reactivation applying the captures as exactly-once
    /// credits). No new event is injected; the effect flows entirely
    /// through the existing consequence mechanism.
    ///
    /// The admission rule (the overlapping-action corrective): ONE
    /// deliberate action at a time — a press during an active
    /// excursion, or while the previous action's captured
    /// consequences still await their target's reactivation, is
    /// refused WITHOUT changing the active action's target or
    /// attribution.
    pub fn excursion(&mut self) -> Result<ActionRecord, String> {
        if !self.session.healthy() {
            return Err(
                "the persistence worker is down — the action refuses (fail closed)".to_string(),
            );
        }
        if self.excursion.is_some() {
            return Err(
                "an excursion is already active — one deliberate action at a time \
                 (the press is refused without changing the active action's target or state)"
                    .to_string(),
            );
        }
        if let Some(action) = &self.action
            && !action.phase.is_terminal()
        {
            return Err(
                "the previous action has not settled — its captured consequences await \
                 the target's reactivation; one deliberate action at a time"
                    .to_string(),
            );
        }
        let region = nearest_region(
            self.session.director().config(),
            self.presence.first().unwrap_or(&DEMO_PRESENCE),
        );
        let (captured_delta, credits_before) = {
            let world = self.session.spine().world();
            (
                region_signal_sum(world, region),
                region_credit_sum(world, region),
            )
        };
        let record = ActionRecord {
            region,
            started_at: self.session.clock().as_u64(),
            excursion_ticks: EXCURSION_TICKS,
            captured_delta,
            credits_before,
            phase: ActionPhase::Excursion,
            credits_after: None,
        };
        self.excursion = Some(ExcursionState {
            region,
            remaining: EXCURSION_TICKS,
        });
        self.action = Some(record.clone());
        Ok(record)
    }

    /// The tracked action record (read-only).
    pub fn action(&self) -> Option<&ActionRecord> {
        self.action.as_ref()
    }

    fn refresh_action(&mut self) {
        let Some(action) = self.action.as_ref() else {
            return;
        };
        // The decision is computed read-only first (the two-phase
        // borrow discipline); the mutation applies it below.
        enum Decision {
            None,
            ZeroCapture {
                credits_after: u64,
            },
            Applied {
                keys: usize,
                revision: Hash,
                credits_now: u64,
            },
            Incomplete {
                reason: String,
            },
        }
        let decision = match &action.phase {
            ActionPhase::Excursion => {
                if self.excursion.is_some() {
                    // Still running — nothing to observe yet.
                    Decision::None
                } else {
                    // The excursion returned without the target's
                    // deactivation (e.g., the target was already
                    // dormant): nothing was captured — an honest no-op
                    // with the observed (unchanged) credits.
                    Decision::ZeroCapture {
                        credits_after: region_credit_sum(
                            self.session.spine().world(),
                            action.region,
                        ),
                    }
                }
            }
            ActionPhase::AwaitingReactivation { keys, revision } => {
                // Completed credit application ONLY after the target
                // actually reactivated AND the applied change matched
                // the captured delta — verified, never assumed; while
                // the target stays dormant the state is honestly
                // pending.
                if self.session.region_level(action.region) == LodLevel::Dormant {
                    Decision::None
                } else {
                    let credits_now =
                        region_credit_sum(self.session.spine().world(), action.region);
                    let applied = credits_now.saturating_sub(action.credits_before);
                    if applied == action.captured_delta {
                        Decision::Applied {
                            keys: *keys,
                            revision: *revision,
                            credits_now,
                        }
                    } else {
                        Decision::Incomplete {
                            reason: format!(
                                "the applied credit change ({applied}) did not match the \
                                 captured delta ({})",
                                action.captured_delta
                            ),
                        }
                    }
                }
            }
            ActionPhase::ZeroCapture
            | ActionPhase::Applied { .. }
            | ActionPhase::Incomplete { .. } => Decision::None,
        };
        match decision {
            Decision::None => {}
            Decision::ZeroCapture { credits_after } => {
                if let Some(action) = self.action.as_mut() {
                    action.phase = ActionPhase::ZeroCapture;
                    action.credits_after = Some(credits_after);
                }
            }
            Decision::Applied {
                keys,
                revision,
                credits_now,
            } => {
                if let Some(action) = self.action.as_mut() {
                    action.phase = ActionPhase::Applied { keys, revision };
                    action.credits_after = Some(credits_now);
                }
                // The action's consequence is confirmed through the
                // required canonical confirmation (the receipt);
                // publish the snapshot (optional, nonblocking) so the
                // world state's own canonical representation also
                // proceeds — the overlay polls it.
                self.publish();
            }
            Decision::Incomplete { reason } => {
                if let Some(action) = self.action.as_mut() {
                    action.phase = ActionPhase::Incomplete { reason };
                }
            }
        }
    }

    /// Captures and submits one optional snapshot publication
    /// (nonblocking admission — a refusal is recorded truthfully and
    /// never an error; the close's required final snapshot guarantees
    /// durability regardless).
    pub fn publish(&mut self) {
        let capture_start = Instant::now();
        let capture = match self.session.capture_snapshot() {
            Ok(capture) => capture,
            Err(error) => {
                let message = format!("capture refused: {error:?}");
                self.emit_persistence(
                    PersistenceKind::Capture,
                    Some(capture_start.elapsed().as_secs_f64() * 1000.0),
                    message.clone(),
                    None,
                );
                self.last_capture_error = Some(message);
                return;
            }
        };
        self.emit_persistence(
            PersistenceKind::Capture,
            Some(capture_start.elapsed().as_secs_f64() * 1000.0),
            "captured".into(),
            None,
        );
        let candidate = capture.content_identity();
        let capture_tick = capture.tick();
        let submit_start = Instant::now();
        match self.session.submit_publication(capture) {
            Ok(handle) => {
                self.emit_persistence(
                    PersistenceKind::Admission,
                    Some(submit_start.elapsed().as_secs_f64() * 1000.0),
                    "admitted".into(),
                    None,
                );
                self.publication = Some(TrackedPublication {
                    handle,
                    capture_tick,
                    submitted_at: Instant::now(),
                    state: PublicationState::Pending { candidate },
                });
                self.last_refusal = None;
            }
            Err(refusal) => {
                self.emit_persistence(
                    PersistenceKind::Busy,
                    Some(submit_start.elapsed().as_secs_f64() * 1000.0),
                    format!("{refusal:?}"),
                    None,
                );
                self.last_refusal = Some(refusal);
            }
        }
    }

    fn pending_handle(&self) -> Option<PublicationHandle> {
        let tracked = self.publication.as_ref()?;
        tracked.state.is_pending().then_some(tracked.handle)
    }

    /// Polls the tracked publication (nonblocking) — the truthful
    /// outcome lands in the status.
    pub fn poll(&mut self) {
        let Some(handle) = self.pending_handle() else {
            return;
        };
        if let Some(outcome) = self.session.poll_publication(handle) {
            self.record_outcome(outcome);
        }
    }

    /// Waits for the tracked publication's completion (blocking — the
    /// headless paths).
    pub fn wait_current(&mut self) {
        let Some(handle) = self.pending_handle() else {
            return;
        };
        if let Some(outcome) = self.session.wait_publication(handle) {
            self.record_outcome(outcome);
        }
    }

    fn record_outcome(&mut self, outcome: PublicationOutcome) {
        // S6-C1: the completion latency — admission (the tracked
        // publication's `submitted_at`) to this observed terminal
        // state, the latency the authority thread actually
        // experiences (queue + append + fsync + readback).
        let completion_latency_ms = self
            .publication
            .as_ref()
            .map(|tracked| tracked.submitted_at.elapsed().as_secs_f64() * 1000.0);
        let state = match outcome {
            PublicationOutcome::Accepted { revision, tier } => {
                self.confirmed_revision = Some(revision);
                PublicationState::Accepted { revision, tier }
            }
            PublicationOutcome::Stale { revision, observed } => {
                PublicationState::Stale { revision, observed }
            }
            PublicationOutcome::Unknown { revision, reason } => {
                PublicationState::Unknown { revision, reason }
            }
            PublicationOutcome::Failed { error, .. } => PublicationState::Failed {
                detail: error.to_string(),
            },
            PublicationOutcome::Canceled { revision } => PublicationState::Canceled { revision },
        };
        let label = state.label();
        let revision = match &state {
            PublicationState::Accepted { revision, .. }
            | PublicationState::Stale { revision, .. }
            | PublicationState::Unknown { revision, .. }
            | PublicationState::Canceled { revision } => Some(revision.to_string()),
            _ => None,
        };
        if let Some(tracked) = self.publication.as_mut() {
            tracked.state = state;
        }
        self.emit_persistence(
            PersistenceKind::Completion,
            completion_latency_ms,
            label,
            revision,
        );
    }

    /// The tracked publication's state (read-only).
    pub fn publication_state(&self) -> Option<&PublicationState> {
        self.publication.as_ref().map(|tracked| &tracked.state)
    }

    /// The per-frame truthful persistence status (behavior *D*).
    pub fn status(&self) -> PersistenceStatus {
        let session = &self.session;
        let ledger = session.director().ledger();
        PersistenceStatus {
            founded: self.founded,
            tick: session.clock(),
            phase_active: session.phase() == SessionPhase::Active,
            worker_healthy: session.healthy(),
            tier: session.worker_tier(),
            pending_publications: session.pending_publications(),
            publication: self
                .publication
                .as_ref()
                .map(|tracked| tracked.state.clone()),
            publication_capture_tick: self
                .publication
                .as_ref()
                .map(|tracked| tracked.capture_tick),
            last_refusal: self.last_refusal,
            last_capture_error: self.last_capture_error.clone(),
            captures_pending: ledger.pending_total(),
            captures_applied: ledger.applied_count() as u64,
            confirmed_revision: self.confirmed_revision,
            last_commit: self.last_commit.clone(),
            action: self.action.clone(),
        }
    }

    /// Observational per-tick orchestrated-boundary duration
    /// percentiles (milliseconds) — the composed gate + transitions +
    /// spine execution as one timed unit, under its own label (never
    /// presented as bare spine time).
    pub fn tick_ms_percentile(&self, quantile: f64) -> f64 {
        if self.tick_ms_history.is_empty() {
            return 0.0;
        }
        let mut sorted = self.tick_ms_history.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let index = ((quantile * sorted.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1);
        sorted[index]
    }

    pub fn tick_sample_count(&self) -> usize {
        self.tick_ms_history.len()
    }

    /// THE single finalization site (HOLD-1): attempt the accepted
    /// graceful close on EVERY exit path after the campaign opened —
    /// normal and application-failed alike — exactly once (the
    /// session is consumed; the structure cannot double-close), with
    /// the original application error preserved alongside the
    /// independent close outcome. The close itself is the accepted
    /// `OrchestratedSession::close` verbatim (the final required
    /// snapshot with readback verification, the SQLite WAL TRUNCATE
    /// lifecycle checkpoint, the settled required confirmations —
    /// `failure == None` the only complete success).
    pub fn finalize(mut self, application_failure: Option<String>) -> Finalization {
        // S6-C1: the evidence collector finalizes AFTER the close — a
        // writer failure can never bypass it — with the close's own
        // duration recorded first (while the writer still runs). The
        // close duration is measured separately from every other
        // latency family (n=1 per run: the full close sequence).
        let evidence = self.evidence.take();
        let closing_tick = self.session.clock().as_u64();
        let close_start = Instant::now();
        let close = self.session.close();
        let close_ms = close_start.elapsed().as_secs_f64() * 1000.0;
        if let Some(ev) = &evidence {
            ev.produce(EvidenceRecord::Persistence(PersistenceSample {
                kind: PersistenceKind::Close,
                wall_offset_ms: ev.wall_offset_ms(),
                tick: closing_tick,
                latency_ms: Some(close_ms),
                outcome: if close.failure.is_none() {
                    "complete".into()
                } else {
                    format!("{:?}", close.failure)
                },
                revision: close.final_revision.map(|revision| revision.to_string()),
            }));
        }
        let evidence_outcome = evidence.map(|ev| ev.finalize());
        Finalization {
            application_failure,
            close,
            evidence: evidence_outcome,
        }
    }
}

// --------------------------------------------------- the graphical loop

/// The demo's camera policy — the Stage 2/4 navigation verbatim (the
/// application owns camera policy, not the renderer). `Space` is the
/// deliberate action key; every historical binding (WASD, mouse look,
/// Q/E roll, R/F vertical, Shift sprint, O overlay, Esc quit) behaves
/// exactly as the render demo's does.
struct Camera {
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    roll: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            position: [0.0, 2.0, 9.0],
            yaw: 0.0,
            pitch: -0.12,
            roll: 0.0,
        }
    }
}

impl Camera {
    fn update(&mut self, snapshot: &InputSnapshot, app: &ShellApp, dt: f32) -> [f32; 3] {
        let look = 0.0025;
        self.yaw += snapshot.look_delta.0 * look;
        self.pitch = (self.pitch - snapshot.look_delta.1 * look).clamp(-1.54, 1.54);
        let roll_input = i8::from(app.is_held(Keycode::E)) - i8::from(app.is_held(Keycode::Q));
        self.roll += roll_input as f32 * 1.5 * dt;
        let speed = if app.is_held(Keycode::LShift) {
            12.0
        } else {
            4.0
        };
        let forward = math::camera_forward(self.yaw, self.pitch, self.roll);
        let right = math::camera_right(self.yaw, self.pitch, self.roll);
        let mut displacement = [0.0f32; 3];
        for axis in 0..3 {
            let delta = forward[axis] * f32::from(snapshot.move_forward) * speed * dt
                + right[axis] * f32::from(snapshot.move_strafe) * speed * dt;
            self.position[axis] += delta;
            displacement[axis] = delta;
        }
        let vertical = i8::from(app.is_held(Keycode::R)) - i8::from(app.is_held(Keycode::F));
        let vertical_delta = vertical as f32 * speed * dt;
        self.position[1] += vertical_delta;
        displacement[1] += vertical_delta;
        displacement
    }
}

/// S6-C1: one memory sample — the platform RSS where the sampler
/// exists (Linux; Windows physical runs use the external sampler —
/// S6-C4) plus the structural dormant occupancy (held obligations per
/// region) and the capture ledger's pending total. Attribution
/// metadata, never an allocation-exact per-NPC cost.
fn emit_memory_evidence(demo: &PersistDemo, evidence: &EvidenceCollector) {
    let session = demo.session();
    let spine = session.spine();
    let mut held_by_region = vec![0u64; REGION_COUNT];
    for view in spine.held_view() {
        let region = usize::from(view.class.0);
        if region < REGION_COUNT {
            held_by_region[region] += 1;
        }
    }
    evidence.produce(EvidenceRecord::Memory(MemorySample {
        wall_offset_ms: evidence.wall_offset_ms(),
        rss_kib: resident_set_kib(),
        held_by_region,
        pending_total: session.director().ledger().pending_total(),
    }));
}

/// Runs the integrated graphical demonstration (behavior *B–E* over
/// the one authoritative persisted session). The loop mirrors the
/// Stage 4 `--render --regions` skeleton exactly — the differences
/// are the orchestrated driver behind it, the `Space` action, the
/// campaign overlay block, and the single finalization site: every
/// exit path after the campaign opened (user quit, `--frames` limit,
/// OR an application-level failure — a refused tick, a render error)
/// runs the graceful durable close, with the original error preserved
/// and the close outcome reported independently.
pub fn run_persist_demo(options: DemoOptions) -> Result<(), String> {
    let shell_config = ShellConfig {
        title: "Sektor7 original engine — Slice 0 integration demo".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    };
    let mut app = ShellApp::new(shell_config)?;
    app.set_relative_mouse(true);

    let render_config = RenderConfig {
        present_mode: options.present,
        validation: options.validation,
        replace_pipelines: false,
    };
    let mut renderer =
        Renderer::new(app.window(), render_config).map_err(|error| error.to_string())?;
    renderer.set_overlay_enabled(options.overlay);

    // S6-C1: the evidence collector starts BEFORE the campaign opens
    // (the open/recovery boundary is the first latency family) and
    // AFTER the renderer (so the device and presentation metadata are
    // known). A start failure is a visible error before any session
    // exists; an open failure finalizes the (empty) package first.
    let collector = match options.evidence.clone() {
        Some(config) => Some(
            EvidenceCollector::start(config)
                .map_err(|error| format!("start the evidence collector: {error}"))?,
        ),
        None => None,
    };
    if let Some(evidence) = &collector
        && options.evidence_fail_writes
    {
        evidence.fault_control().set_fail_writes(true);
    }

    // The campaign: found-and-anchor, or recover (fail closed on
    // corrupt/divergent/unverifiable state — never an overwrite).
    let timing = SimConfig::default();
    let open_start = Instant::now();
    let (mut demo, launch) = match PersistDemo::open_or_recover(&options.campaign_dir, timing) {
        Ok(opened) => opened,
        Err(error) => {
            if let Some(evidence) = collector {
                let _ = evidence.finalize();
            }
            return Err(error);
        }
    };
    if let Some(evidence) = collector {
        {
            let stats = renderer.stats();
            evidence.note_metadata("mode", "graphical");
            evidence.note_metadata("platform", std::env::consts::OS);
            evidence.note_metadata("gpu", &stats.gpu_name);
            evidence.note_metadata("device_type", &stats.device_type);
            evidence.note_metadata("vulkan_api", &stats.api_version);
            evidence.note_metadata("present_mode", &stats.present_mode);
            evidence.note_metadata("extent", &format!("{}x{}", stats.extent.0, stats.extent.1));
            evidence.note_metadata("swapchain_images", &stats.swapchain_images.to_string());
            evidence.note_metadata("validation_active", &stats.validation_active.to_string());
            evidence.note_metadata("fps_target", &options.fps.to_string());
        }
        evidence.produce(EvidenceRecord::Persistence(PersistenceSample {
            kind: PersistenceKind::Recovery,
            wall_offset_ms: evidence.wall_offset_ms(),
            tick: demo.start_tick(),
            latency_ms: Some(open_start.elapsed().as_secs_f64() * 1000.0),
            outcome: if launch.founded {
                "founded+anchor".into()
            } else {
                "recovered".into()
            },
            revision: launch.anchor_revision.map(|revision| revision.to_string()),
        }));
        demo.attach_evidence(evidence);
    }

    {
        let stats = renderer.stats();
        println!("Sektor7 Slice 0 — integrated interactive persistence demonstration");
        println!(
            "  campaign: {} · db {} · journal {}",
            if launch.founded {
                "founded"
            } else {
                "recovered"
            },
            launch.database.display(),
            launch.journal.display()
        );
        if let Some(anchor) = launch.anchor_revision {
            println!("  recovery anchor verified: {anchor}");
        }
        if let (Some(tick), fingerprint) = (
            launch.recovered_at_tick,
            launch.recovered_fingerprint.as_deref(),
        ) {
            println!(
                "  recovered at tick {} · restored fingerprint {}",
                tick.as_u64(),
                fingerprint.unwrap_or("?")
            );
        }
        println!(
            "  device: {} {} (Vulkan {})",
            stats.device_type, stats.gpu_name, stats.api_version
        );
        println!(
            "  present: {} (supported: {}) · {} swapchain images · {}x{} · validation: {}",
            stats.present_mode,
            stats.supported_present_modes.join("/"),
            stats.swapchain_images,
            stats.extent.0,
            stats.extent.1,
            stats.validation_active
        );
        println!(
            "  controls: WASD move · mouse look · Q/E roll · R/F up/down · \
             Shift fast · Space action (the region's durable consequence \
             cycle — one at a time) · O overlay · Esc/close quit (runs the \
             graceful durable close)"
        );
    }

    let mut camera = Camera {
        position: [100.0, 2.0, 9.0],
        ..Camera::default()
    };
    let mut cpu_work_ms: Vec<f64> = Vec::new();
    let mut frame_intervals_ms: Vec<f64> = Vec::new();
    let mut presented: u64 = 0;
    // S6-C1: the evidence bookkeeping — the loop-iteration sample
    // index and the memory cadence anchor (1 s wall).
    let mut evidence_sample_index: u64 = 0;
    let mut last_memory_sample: Option<Instant> = None;
    // Each accepted action's completion is reported exactly once,
    // keyed by its start tick (a subsequent action reports
    // independently).
    let mut action_completion_printed_at: Option<u64> = None;
    // The application-level failure captured from the loop (HOLD-1):
    // a refused tick or a render error breaks OUT of the loop into
    // the single finalization site instead of bypassing the close.
    let mut loop_failure: Option<String> = None;
    let start = Instant::now();
    let frame_interval = if options.fps == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(
            1.0 / f64::from(options.fps.clamp(1, 1000)),
        ))
    };
    let mut frame_timing = FrameTiming::new();

    'run: loop {
        let frame_start = Instant::now();
        let input_start = frame_start;
        let snapshot = app.frame();
        if snapshot.quit_requested || snapshot.keys_pressed.contains(&Keycode::Escape) {
            break;
        }
        if snapshot.keys_pressed.contains(&Keycode::O) {
            renderer.set_overlay_enabled(!renderer.overlay_enabled());
        }
        // THE deliberate action: one keypress → the region's full
        // durable consequence cycle (behavior *C*). A refusal (the
        // worker down, or an action still active/unsettled) is
        // reported and the run continues — the next tick fails closed
        // if the gate is genuinely down.
        if snapshot.keys_pressed.contains(&Keycode::Space) {
            match demo.excursion() {
                Ok(record) => println!("action started: {}", record.describe()),
                Err(message) => println!("action refused: {message}"),
            }
        }
        // S6-C1: the input phase — the event pump plus the key
        // handling above, the first of the separately labeled frame
        // phases.
        let input_ms = input_start.elapsed().as_secs_f64() * 1000.0;

        let split = frame_timing.begin_frame(frame_start, !snapshot.minimized);
        frame_intervals_ms.push((split.sim_advance.as_secs_f64() * 1000.0).min(100.0));
        let len = frame_intervals_ms.len();
        if len > 600 {
            frame_intervals_ms.drain(0..len - 600);
        }
        evidence_sample_index += 1;

        if snapshot.minimized {
            // The campaign clock keeps advancing while the application
            // runs — no silent pause policy (the accepted discipline).
            let sim_start = Instant::now();
            if let Err(error) = demo.advance(split.sim_advance) {
                loop_failure = Some(format!("the authoritative tick refused: {error:?}"));
                break 'run;
            }
            let sim_ms = sim_start.elapsed().as_secs_f64() * 1000.0;
            if let Some(evidence) = demo.evidence() {
                evidence.produce(EvidenceRecord::Frame(FrameSample {
                    sample: evidence_sample_index,
                    wall_offset_ms: evidence.wall_offset_ms(),
                    interval_ms: split.sim_advance.as_secs_f64() * 1000.0,
                    presented: false,
                    recreated: false,
                    deferred: false,
                    cpu_work_ms: None,
                    input_ms: Some(input_ms),
                    sim_ms: Some(sim_ms),
                    render_ms: None,
                    poll_ms: None,
                    gpu_ms: None,
                }));
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let camera_dt = split.camera_dt.unwrap_or_default().as_secs_f32();
        let camera_displacement = camera.update(&snapshot, &app, camera_dt);
        demo.set_presence(vec![camera_presence(
            camera.position,
            camera_displacement,
            camera_dt,
        )]);

        // Fixed-step advance of the ONE authoritative session (the
        // camera is the live, frame-sampled presence source — the same
        // genuinely cadence-dependent external input as `--regions`;
        // the deliberate action's excursion overrides it while
        // active).
        let sim_start = Instant::now();
        if let Err(error) = demo.advance(split.sim_advance) {
            loop_failure = Some(format!("the authoritative tick refused: {error:?}"));
            break 'run;
        }
        let sim_ms = sim_start.elapsed().as_secs_f64() * 1000.0;
        // S6-C1: the persistence poll phase — the tracked
        // publication's nonblocking check.
        let poll_start = Instant::now();
        demo.poll();
        let poll_ms = poll_start.elapsed().as_secs_f64() * 1000.0;

        if let Some(action) = demo.action()
            && action.phase.is_terminal()
            && action_completion_printed_at != Some(action.started_at)
        {
            action_completion_printed_at = Some(action.started_at);
            println!("action completed: {}", action.describe());
        }

        // S6-C1: the render phase — the derived scene, the overlay
        // text, and the frame submission (CPU-side; the GPU duration
        // arrives separately through the timestamp result).
        let render_start = Instant::now();
        // The derived scene: read-only over the authoritative world
        // (rendering stays strictly downstream of authoritative
        // state — the excursion's LOD cycle is visible as the plates
        // and proxies change).
        let region_scene = {
            let session = demo.session();
            build_region_scene(
                &session.director().levels(),
                session.spine().world(),
                session.director().config(),
            )
        };

        let (width, height) = renderer.stats().extent;
        let aspect = if height == 0 {
            16.0 / 9.0
        } else {
            width as f32 / height as f32
        };
        let view = math::camera_view(camera.position, camera.yaw, camera.pitch, camera.roll);
        let projection = math::perspective(std::f32::consts::FRAC_PI_3, aspect, 0.1, 400.0);
        let view_proj = math::mul(&projection, &view);

        let overlay_text = renderer.overlay_enabled().then(|| {
            let elapsed = start.elapsed().as_secs_f64().max(0.001);
            let stats = renderer.stats();
            let session = demo.session();
            let spine = session.spine();
            let sim_stats = spine.stats();
            let mut text = format!(
                "Sektor7 Slice 0 - integration demo\nFPS {:.0} (cpu work p95 {:.2} ms / gpu p95 {:.2} ms)\npresent {} / {}x{} / {} images / recreations {}\n{} {} / Vulkan {}\nvalidation {} (err {} / warn {})\nsim tick {} @ {} Hz · out {} · ev/s {:.0}\nnear/far {}/{} · pend {} · blk {} · late {} · bud-exh {}\nwm {} / {}\ncam ({:.1}, {:.1}, {:.1}) yaw {:.2} pitch {:.2}",
                presented as f64 / elapsed,
                math::percentile(&cpu_work_ms, 0.95),
                stats.gpu_ms_p95(),
                stats.present_mode,
                width,
                height,
                stats.swapchain_images,
                stats.swapchain_recreations,
                stats.device_type,
                stats.gpu_name,
                stats.api_version,
                stats.validation_active,
                stats.validation_errors,
                stats.validation_warnings,
                spine.clock(),
                demo.config().tick_rate_hz,
                demo.outstanding_ticks(),
                sim_stats.executed as f64 / elapsed,
                spine.near_pending_count(),
                spine.far_pending_count(),
                spine.pending_count(),
                spine.blocked_count(),
                sim_stats.late_executions,
                sim_stats.budget_exhausted_ticks,
                spine
                    .processed_through()
                    .map(|tick| tick.as_u64().to_string())
                    .unwrap_or_else(|| "none".into()),
                spine
                    .max_executed_due()
                    .map(|tick| tick.as_u64().to_string())
                    .unwrap_or_else(|| "none".into()),
                camera.position[0],
                camera.position[1],
                camera.position[2],
                camera.yaw,
                camera.pitch,
            );
            let director = session.director();
            let diagnostics = director.diagnostics();
            text.push_str(&format!(
                "\nregions {}\nact {} · deact {} · invalid {} · recon {}\ncaptured {} · reapplied {} · act-lat p50 {:.2} / p95 {:.2} ms",
                levels_compact(&director.levels()),
                diagnostics.activations,
                diagnostics.deactivations,
                diagnostics.invalid_transitions,
                diagnostics.reconstructed_activities,
                diagnostics.captured_deltas,
                diagnostics.reapplied_deltas,
                director.activation_latency_percentile(0.50),
                director.activation_latency_percentile(0.95),
            ));
            // The truthful campaign block (behavior *D*): every line
            // derives from the actual lifecycle surfaces.
            let status = demo.status();
            text.push_str(&format!(
                "\n--- campaign ---\n{} · worker {} · tier {:?}\ntick {} · phase {} · captures pending {} · applied {}\npub pending {} @ {} · last {}\nrefusal {:?} · capture-err {}\ncommit {}\nconfirmed {}\naction {}",
                if status.founded { "founded" } else { "recovered" },
                if status.worker_healthy { "healthy" } else { "DOWN" },
                status.tier,
                status.tick.as_u64(),
                if status.phase_active { "active" } else { "SUSPENDED" },
                status.captures_pending,
                status.captures_applied,
                status.pending_publications,
                status
                    .publication_capture_tick
                    .map(|tick| tick.to_string())
                    .unwrap_or_else(|| "-".into()),
                status
                    .publication
                    .as_ref()
                    .map(|state| state.label())
                    .unwrap_or_else(|| "none".into()),
                status.last_refusal,
                status.last_capture_error.as_deref().unwrap_or("none"),
                status.last_commit.as_deref().unwrap_or("none yet"),
                status
                    .confirmed_revision
                    .map(|revision| revision.to_string())
                    .unwrap_or_else(|| "none yet".into()),
                status
                    .action
                    .as_ref()
                    .map(|record| record.describe())
                    .unwrap_or_else(|| "idle — Space starts the region's consequence cycle".into()),
            ));
            text
        });

        let frame_result = match renderer.render_frame(
            &view_proj,
            &region_scene,
            overlay_text.as_deref(),
            app.window_size(),
        ) {
            Ok(result) => result,
            Err(error) => {
                loop_failure = Some(format!("{error}"));
                break 'run;
            }
        };
        let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;

        // S6-C1: the per-iteration frame sample — raw (unclamped)
        // values, the presented/recreated/deferred flags, the four
        // CPU phases, and the GPU timestamp result verbatim (`null`
        // when unsupported — never a substituted zero). The memory
        // cadence rides the same guard (1 s wall).
        if let Some(evidence) = demo.evidence() {
            evidence.produce(EvidenceRecord::Frame(FrameSample {
                sample: evidence_sample_index,
                wall_offset_ms: evidence.wall_offset_ms(),
                interval_ms: split.sim_advance.as_secs_f64() * 1000.0,
                presented: frame_result.presented,
                recreated: frame_result.recreated,
                deferred: frame_result.deferred,
                cpu_work_ms: frame_result
                    .presented
                    .then(|| frame_start.elapsed().as_secs_f64() * 1000.0),
                input_ms: Some(input_ms),
                sim_ms: Some(sim_ms),
                render_ms: Some(render_ms),
                poll_ms: Some(poll_ms),
                gpu_ms: frame_result.gpu_render_ms,
            }));
            let sample_memory = match last_memory_sample {
                None => true,
                Some(last) => last.elapsed() >= Duration::from_secs(1),
            };
            if sample_memory {
                emit_memory_evidence(&demo, evidence);
                last_memory_sample = Some(Instant::now());
            }
        }

        if frame_result.deferred {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        if frame_result.presented {
            presented += 1;
            cpu_work_ms.push((frame_start.elapsed().as_secs_f64() * 1000.0).min(1000.0));
            let len = cpu_work_ms.len();
            if len > 600 {
                cpu_work_ms.drain(0..len - 600);
            }
        }
        if let Some(limit) = options.frames
            && presented >= limit
        {
            break;
        }
        if let Some(interval) = frame_interval {
            let elapsed = frame_start.elapsed();
            if elapsed < interval {
                std::thread::sleep(interval - elapsed);
            }
        }
    }

    let elapsed = start.elapsed();
    let stats = renderer.stats();
    if let Some(failure) = &loop_failure {
        println!("--- APPLICATION FAILURE (the run exits through the finalization) ---");
        println!("  {failure}");
    }
    println!("--- run summary ---");
    println!(
        "presented {} frames over {:.1}s ({:.1} fps avg)",
        presented,
        elapsed.as_secs_f64(),
        presented as f64 / elapsed.as_secs_f64().max(0.001)
    );
    println!(
        "cpu work (poll→present, excl. pacing sleep): p50 {:.2} ms · p95 {:.2} ms · p99 {:.2} ms",
        math::percentile(&cpu_work_ms, 0.50),
        math::percentile(&cpu_work_ms, 0.95),
        math::percentile(&cpu_work_ms, 0.99)
    );
    println!(
        "gpu render (timestamps, {} samples): p50 {:.2} ms · p95 {:.2} ms · p99 {:.2} ms",
        stats.gpu_sample_count(),
        stats.gpu_ms_p50(),
        stats.gpu_ms_p95(),
        stats.gpu_ms_p99()
    );
    println!(
        "present mode {} · swapchain recreations {} · validation err/warn {}/{}",
        stats.present_mode,
        stats.swapchain_recreations,
        stats.validation_errors,
        stats.validation_warnings
    );
    println!("--- simulation summary (the one authoritative session) ---");
    {
        let session = demo.session();
        let spine = session.spine();
        let sim_stats = spine.stats();
        let totals = spine.lane_totals();
        println!(
            "config: {} Hz tick rate · budget {} · near reserve {} · catch-up {}/frame",
            demo.config().tick_rate_hz,
            spine.budget(),
            demo.config().near_reserve_description(),
            demo.config().max_catchup_per_frame
        );
        // The ABSOLUTE time accounting (HOLD-2): the recovered starting
        // clock included, so the conservation equation is correct for
        // founded and recovered campaigns alike.
        println!(
            "sim time: advanced {:.1} ms this session · campaign ticks due {} = start {} + session {} (absolute, never discarded)",
            frame_timing.total_sim_advanced().as_secs_f64() * 1000.0,
            demo.ticks_due_total(),
            demo.start_tick(),
            demo.session_ticks_due()
        );
        println!(
            "conservation: due {} == campaign tick {} + outstanding {}",
            demo.ticks_due_total(),
            spine.clock(),
            demo.outstanding_ticks()
        );
        println!(
            "campaign tick {} · executed {} · late arrivals {} · late executions {}",
            spine.clock(),
            sim_stats.executed,
            sim_stats.late_arrivals,
            sim_stats.late_executions
        );
        println!(
            "backlog pending {} (near {} / far {}) · blocked {} · budget-exhausted ticks {}",
            spine.pending_count(),
            spine.near_pending_count(),
            spine.far_pending_count(),
            spine.blocked_count(),
            sim_stats.budget_exhausted_ticks
        );
        println!(
            "lanes near {} (backlogged ticks {}) · far {} (backlogged ticks {})",
            totals.near_executed,
            totals.near_backlogged_ticks,
            totals.far_executed,
            totals.far_backlogged_ticks
        );
        println!(
            "orchestrated tick time (gate + transitions + spine): p50 {:.3} ms · p95 {:.3} ms (per-tick, {} samples)",
            demo.tick_ms_percentile(0.50),
            demo.tick_ms_percentile(0.95),
            demo.tick_sample_count()
        );
        println!(
            "watermark max_executed_due {} · processed_through {}",
            spine
                .max_executed_due()
                .map(|tick| tick.as_u64().to_string())
                .unwrap_or_else(|| "none".into()),
            spine
                .processed_through()
                .map(|tick| tick.as_u64().to_string())
                .unwrap_or_else(|| "none".into())
        );
        println!(
            "npc activity steps {} · fingerprints engine-lanes {} · state-lanes {}",
            spine.world().total_steps(),
            spine.lane_fingerprint_hex(),
            spine.lane_state_fingerprint_hex()
        );
    }
    println!("--- regions ---");
    {
        let session = demo.session();
        let director = session.director();
        let diagnostics = director.diagnostics();
        println!("  levels: {}", levels_compact(&director.levels()));
        println!(
            "  transitions: activations {} · deactivations {} · invalid {}",
            diagnostics.activations, diagnostics.deactivations, diagnostics.invalid_transitions
        );
        println!(
            "  reconstruction: activities {} · replay {} / synthesis {} · captured {} · reapplied {}",
            diagnostics.reconstructed_activities,
            diagnostics.replay_activations,
            diagnostics.synthesis_activations,
            diagnostics.captured_deltas,
            diagnostics.reapplied_deltas
        );
        println!(
            "  activation latency (observational): p50 {:.2} ms · p95 {:.2} ms · worst {:.2} ms",
            director.activation_latency_percentile(0.50),
            director.activation_latency_percentile(0.95),
            director.activation_latency_percentile(1.0)
        );
        println!(
            "  deactivation latency (observational): p50 {:.2} ms · p95 {:.2} ms · worst {:.2} ms",
            director.deactivation_latency_percentile(0.50),
            director.deactivation_latency_percentile(0.95),
            director.deactivation_latency_percentile(1.0)
        );
        println!(
            "  fingerprints: stage4 {} · held {}",
            stage4_state_fingerprint_hex(session.spine(), director),
            session.spine().held_fingerprint_hex()
        );
    }
    println!("--- campaign ---");
    if let Some(record) = demo.action() {
        println!("  action: {}", record.describe());
    } else {
        println!("  action: none this run (Space triggers it)");
    }
    if let Some(state) = demo.publication_state() {
        println!("  last publication: {}", state.label());
    }
    let status = demo.status();
    println!(
        "  captures: pending {} · applied {} · last commit {}",
        status.captures_pending,
        status.captures_applied,
        status.last_commit.as_deref().unwrap_or("none yet")
    );
    println!(
        "  confirmed revision: {}",
        status
            .confirmed_revision
            .map(|revision| revision.to_string())
            .unwrap_or_else(|| "none yet".into())
    );

    // THE SINGLE FINALIZATION SITE (HOLD-1): every exit path — the
    // user quit, the `--frames` limit, or the captured application
    // failure above — runs the accepted graceful close exactly once,
    // with the original error preserved and the close outcome
    // reported independently. A rendering failure never leaves the
    // campaign unclosed, and a failed close is never represented as
    // successful.
    println!("--- campaign close ---");
    let finalization = demo.finalize(loop_failure);
    let report = &finalization.close;
    println!(
        "  final revision: {}",
        report
            .final_revision
            .map(|revision| revision.to_string())
            .unwrap_or_else(|| "none (no snapshot verified)".into())
    );
    println!("  final tier: {:?}", report.final_tier);
    println!(
        "  lifecycle checkpoint: {} · required settled: {}",
        report.lifecycle_checkpoint, report.required_settled
    );
    println!("  completions: {}", report.completions.len());
    match &report.worker {
        Ok(()) => println!("  worker join: clean"),
        Err(error) => println!("  worker join: {error}"),
    }
    match &report.failure {
        None => println!(
            "  close: COMPLETE — verified final snapshot + lifecycle checkpoint + settled confirmations + clean join"
        ),
        Some(failure) => println!(
            "  close: FAILED — {failure:?} (the campaign remains recoverable from its durable state)"
        ),
    }
    if let Some(outcome) = &finalization.evidence {
        println!("--- evidence ---");
        println!("{}", outcome.summary());
    }
    if report.failure.is_none() && finalization.application_failure.is_none() {
        println!(
            "relaunch with the same --demo-persist directory to recover this campaign \
             (no input replay, no duplicated consequences)"
        );
    }
    // The combined outcome (HOLD-1): the original application error
    // first, the close outcome independent, never a success claim
    // over a failed close — then the validation findings merged in.
    let mut outcome = finalization.combined();
    for message in &stats.recent_validation {
        println!("  validation: {message}");
    }
    if stats.validation_active && stats.validation_errors > 0 {
        let validation_error = format!(
            "validation layer reported {} error(s) while active — see the messages above",
            stats.validation_errors
        );
        outcome = Err(match outcome {
            Err(existing) => format!("{existing}; AND {validation_error}"),
            Ok(()) => validation_error,
        });
    }
    if outcome.is_err() {
        println!(
            "note: instrument readings of this demonstration scene — not \
             first-game performance claims; software-ICD runs are correctness \
             smoke only, and physical graphical acceptance belongs to the \
             owner's rig."
        );
    }
    outcome
}

/// Runs the demonstration headlessly: the same campaign lifecycle,
/// tick discipline, action, publication, and close over the fixed
/// [`DEMO_PRESENCE`] (the deliberate action's excursion overrides it
/// while active) — the deterministic evidence path (no GPU, no
/// window; graphical verification belongs to physical hardware).
/// Errors after the campaign opened (a refused tick or action) do
/// NOT bypass the close: the run finalizes through the single site
/// and the report carries the preserved application failure
/// alongside the independent close outcome.
pub fn run_persist_demo_headless(options: DemoOptions) -> Result<HeadlessReport, String> {
    let timing = SimConfig::default();
    // S6-C1: the collector starts BEFORE the campaign opens — the
    // open/recovery boundary is the first latency family. A start
    // failure is a visible error before any session exists (nothing
    // to close); an open failure after the collector started
    // finalizes the (empty) evidence package before returning.
    let collector = match options.evidence.clone() {
        Some(config) => Some(
            EvidenceCollector::start(config)
                .map_err(|error| format!("start the evidence collector: {error}"))?,
        ),
        None => None,
    };
    if let Some(evidence) = &collector
        && options.evidence_fail_writes
    {
        evidence.fault_control().set_fail_writes(true);
    }
    let open_start = Instant::now();
    let (mut demo, launch) = match PersistDemo::open_or_recover(&options.campaign_dir, timing) {
        Ok(opened) => opened,
        Err(error) => {
            if let Some(evidence) = collector {
                let _ = evidence.finalize();
            }
            return Err(error);
        }
    };
    if let Some(evidence) = collector {
        evidence.note_metadata("mode", "headless");
        evidence.note_metadata("platform", std::env::consts::OS);
        evidence.note_metadata("headless_ticks", &options.headless_ticks.to_string());
        evidence.produce(EvidenceRecord::Persistence(PersistenceSample {
            kind: PersistenceKind::Recovery,
            wall_offset_ms: evidence.wall_offset_ms(),
            tick: demo.start_tick(),
            latency_ms: Some(open_start.elapsed().as_secs_f64() * 1000.0),
            outcome: if launch.founded {
                "founded+anchor".into()
            } else {
                "recovered".into()
            },
            revision: launch.anchor_revision.map(|revision| revision.to_string()),
        }));
        demo.attach_evidence(evidence);
    }
    let mut application_failure: Option<String> = None;
    for tick_index in 0..options.headless_ticks {
        if options.action_at == Some(tick_index) {
            match demo.excursion() {
                Ok(record) => println!("action @ tick {tick_index}: {}", record.describe()),
                Err(message) => {
                    application_failure = Some(format!(
                        "the action refused at tick {tick_index}: {message}"
                    ));
                    break;
                }
            }
        }
        // The HOLD-1 regression's controlled seam: poison the worker
        // at the named tick — the next tick refuses through the
        // composed gate, exercising the error path's finalization
        // deterministically (never set by the binary).
        if options.poison_at_tick == Some(tick_index) {
            demo.session().poison();
        }
        if let Err(error) = demo.run_headless(1) {
            application_failure = Some(format!("tick {tick_index} refused: {error:?}"));
            break;
        }
    }
    // Complete the tracked publication (the headless paths' blocking
    // wait) before reading the evidence — the report's publication
    // state must be the verified outcome, not an unpolled pending.
    demo.wait_current();
    // The evidence, read before the finalization consumes the session.
    let action = demo.action().cloned();
    let action_publication = demo.publication_state().cloned();
    let closed_at_tick = demo.session().clock();
    let closed_ticks_due = demo.ticks_due_total();
    let closed_outstanding = u64::from(demo.outstanding_ticks());
    let closed_fingerprint =
        stage4_state_fingerprint_hex(demo.session().spine(), demo.session().director());
    let closed_region = demo.action().map(|record| {
        let world = demo.session().spine().world();
        (
            record.region,
            region_signal_sum(world, record.region),
            region_credit_sum(world, record.region),
        )
    });
    // THE SINGLE FINALIZATION SITE (HOLD-1) — every path.
    let finalization = demo.finalize(application_failure);
    let close = finalization.close_summary();
    Ok(HeadlessReport {
        launch,
        application_failure: finalization.application_failure.clone(),
        action,
        action_publication,
        closed_at_tick,
        closed_ticks_due,
        closed_outstanding,
        closed_fingerprint,
        closed_region,
        close,
        evidence: finalization.evidence,
    })
}
