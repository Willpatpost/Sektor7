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
//! obligations are retained and reported, never discarded.
//!
//! *C — one deliberate player action.* `Space` starts a bounded
//! PRESENCE EXCURSION on the camera's region: the region the player
//! stands in is driven through the accepted PP-B4 consequence
//! lifecycle — the durable deactivation (its NPCs' accrued uncommitted
//! consequential signals captured under the T1 journal before the
//! discard), the required canonical confirmation (the `Confirmed`
//! receipt with the consequence keys), and the reactivation (the
//! captures applied as exactly-once credits on those NPCs' durable
//! state). The effect is a real, durable, observable NPC consequence
//! through the existing consequence mechanism — no new event is
//! injected (the workload's one-pending-routine-chain-per-NPC
//! discipline is exactly what the accepted deactivation-plan codec
//! validates), and the consequence survives close/recovery by the
//! accepted exactly-once identities.
//!
//! *D — truthful persistence state.* The overlay reports the actual
//! lifecycle: the excursion's live-but-unconfirmed captures, pending
//! publications (admitted, outcome unverified), `Accepted` revisions
//! with the provider's tier, stale/unknown/failed/canceled
//! dispositions, the T1-capture counts, and the required-confirmation
//! outcomes. Nothing is ever called "saved" without a verified
//! receipt.
//!
//! *E — quit and restart.* Exit runs the graceful durable close (the
//! final required snapshot with readback verification, the SQLite WAL
//! TRUNCATE lifecycle checkpoint, the settled required confirmations);
//! a relaunch recovers exactly — no input replay, no duplicated
//! consequence (the accepted exactly-once identities and the recovery
//! machinery guarantee it; the integration tests prove it against the
//! never-persisted twin).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine_core::{PresenceSample, Spine, Tick};
use engine_persist::{
    AuthorityMode, CanonicalMedium, CommitOutcome, Hash, RevisionRecord, SyncTier, T1Journal,
    UnknownReason,
};
use engine_renderer::{PresentModePreference, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{DriverPreference, InputSnapshot, Keycode, ShellApp, ShellConfig};

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
/// live presence (the camera) resumes and the reactivation applies
/// the captured credits.
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

/// The excursion's canonically confirmed capture (from the required
/// confirmation's receipt — the verified evidence, never a guess).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirmation {
    /// The confirmed consequence keys (the captured NPCs' identities).
    pub keys: usize,
    /// The confirmed revision on the canonical chain.
    pub revision: Hash,
}

/// One deliberate action's full record: the target region, the
/// excursion window, the consequence's before-state, the confirmed
/// capture, and the applied credits after the return.
#[derive(Debug, Clone)]
pub struct ActionRecord {
    pub region: usize,
    /// The tick index the excursion started at.
    pub started_at: u64,
    pub excursion_ticks: u64,
    /// The region's uncommitted signal scratch before the excursion —
    /// the consequence that will be captured and confirmed.
    pub signals_before: u64,
    /// The region's committed credits before the excursion.
    pub credits_before: u64,
    /// The deactivation's canonically confirmed capture — `Some` once
    /// the excursion's required confirmation returns `Confirmed` (the
    /// verified receipt; `None` for an empty-scratch deactivation,
    /// which journals nothing by the accepted rule).
    pub confirmed: Option<Confirmation>,
    /// The region's credits after the reactivation — `Some` once the
    /// excursion has fully returned (the exactly-once application
    /// observed, never claimed before it happens).
    pub credits_after: Option<u64>,
}

impl ActionRecord {
    /// The overlay/print form — the truthful progressive states, never
    /// claiming confirmation or application before it is observed.
    pub fn describe(&self) -> String {
        let confirmed = self
            .confirmed
            .as_ref()
            .map(|confirmation| format!("{} keys @ {}", confirmation.keys, confirmation.revision))
            .unwrap_or_else(|| "pending".into());
        let credits_after = self
            .credits_after
            .map(|credits| credits.to_string())
            .unwrap_or_else(|| "…".into());
        format!(
            "excursion r{} @ tick {} ({} ticks) — signals {} · confirmed {} · credits {}→{}",
            self.region,
            self.started_at,
            self.excursion_ticks,
            self.signals_before,
            confirmed,
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
    /// A recovered campaign's restored clock.
    pub recovered_at_tick: Option<Tick>,
    /// A recovered campaign's restored stage-4 fingerprint.
    pub recovered_fingerprint: Option<String>,
}

struct TrackedPublication {
    handle: PublicationHandle,
    /// The capture's simulation tick — the pending display's honest
    /// provenance (when the unverified state was captured).
    capture_tick: u64,
    state: PublicationState,
}

/// The deliberate action's in-progress excursion state.
struct ExcursionState {
    region: usize,
    remaining: u64,
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
    pub action: Option<ActionRecord>,
    pub action_publication: Option<PublicationState>,
    pub closed_at_tick: Tick,
    /// The stage-4 state fingerprint at close (before the close's own
    /// snapshot — the state the close then durably represents).
    pub closed_fingerprint: String,
    /// The action region's (signals, credits) sums at close — the
    /// durable-outcome witness for the deliberate consequence.
    pub closed_region: Option<(usize, u64, u64)>,
    pub close: CloseSummary,
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
            "  closed at tick {} · stage4 fingerprint {}",
            self.closed_at_tick.as_u64(),
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
}

impl PersistDemo {
    fn empty(session: OrchestratedSession, timing: SimConfig, period_ns: u128) -> Self {
        PersistDemo {
            session,
            timing,
            period_ns,
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

    /// Accrued due-but-unexecuted ticks right now (retained
    /// obligation — reported, never discarded).
    pub fn outstanding_ticks(&self) -> u32 {
        (self.accumulator_ns / self.period_ns) as u32
    }

    /// Ticks due since start, from every source (the no-discard
    /// accounting invariant, mirroring `SimDriver`).
    pub fn ticks_due_total(&self) -> u64 {
        (self.total_advanced_ns / self.period_ns) as u64 + self.headless_ticks
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
        self.run_ticks(executed)?;
        self.accumulator_ns -= u128::from(executed) * self.period_ns;
        Ok(DemoStep {
            executed_ticks: executed,
            outstanding_ticks: due - executed,
            campaign_tick: self.session.clock(),
        })
    }

    /// Runs exactly `ticks` fixed ticks — the headless deterministic
    /// path (no real-time input at all).
    pub fn run_headless(&mut self, ticks: u64) -> Result<(), TransitionError> {
        self.headless_ticks += ticks;
        self.run_ticks(ticks as u32)
    }

    fn run_ticks(&mut self, ticks: u32) -> Result<(), TransitionError> {
        for _ in 0..ticks {
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
            let presence: Vec<PresenceSample> = match antipode {
                Some(sample) => vec![sample],
                None => self.presence.clone(),
            };
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
            if !report.regions_transitioned.is_empty() {
                // A capture-bearing boundary occurred: drive the
                // required canonical confirmation (the tests'
                // discipline — T1 durability precedes it, and the
                // outcome label lands in the overlay).
                let outcome = self.session.commit_canonical()?;
                self.last_commit = Some(commit_label(&outcome));
                if let CommitOutcome::Confirmed(receipt) = &outcome {
                    self.confirmed_revision = Some(receipt.revision);
                    if during_excursion
                        && let Some(action) = &mut self.action
                        && action.confirmed.is_none()
                    {
                        action.confirmed = Some(Confirmation {
                            keys: receipt.keys.len(),
                            revision: receipt.revision,
                        });
                    }
                }
            }
            self.refresh_action();
        }
        Ok(())
    }

    /// THE deliberate player action (behavior *C*): starts a bounded
    /// presence excursion on the region the presence stands in — the
    /// region is driven through the accepted consequence lifecycle
    /// (durable deactivation with its NPCs' uncommitted signals
    /// captured under T1, the required canonical confirmation, and
    /// the reactivation applying the captures as exactly-once
    /// credits). No new event is injected; the effect flows entirely
    /// through the existing consequence mechanism.
    pub fn excursion(&mut self) -> Result<ActionRecord, String> {
        if !self.session.healthy() {
            return Err(
                "the persistence worker is down — the action refuses (fail closed)".to_string(),
            );
        }
        let region = nearest_region(
            self.session.director().config(),
            self.presence.first().unwrap_or(&DEMO_PRESENCE),
        );
        let (signals_before, credits_before) = {
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
            signals_before,
            credits_before,
            confirmed: None,
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
        let Some(action) = &self.action else { return };
        if action.credits_after.is_some() {
            return;
        }
        // The application is observed only after the excursion has
        // fully returned (the reactivation tick ran — the excursion
        // state cleared) — never claimed before it happens.
        if self.excursion.is_some() {
            return;
        }
        let region = action.region;
        let credits_after = region_credit_sum(self.session.spine().world(), region);
        if let Some(action) = self.action.as_mut() {
            action.credits_after = Some(credits_after);
        }
        // The action's consequence is confirmed through the required
        // canonical confirmation (the receipt); publish the snapshot
        // (optional, nonblocking) so the world state's own canonical
        // representation also proceeds — the overlay polls it.
        self.publish();
    }

    /// Captures and submits one optional snapshot publication
    /// (nonblocking admission — a refusal is recorded truthfully and
    /// never an error; the close's required final snapshot guarantees
    /// durability regardless).
    pub fn publish(&mut self) {
        let capture = match self.session.capture_snapshot() {
            Ok(capture) => capture,
            Err(error) => {
                self.last_capture_error = Some(format!("capture refused: {error:?}"));
                return;
            }
        };
        let candidate = capture.content_identity();
        let capture_tick = capture.tick();
        match self.session.submit_publication(capture) {
            Ok(handle) => {
                self.publication = Some(TrackedPublication {
                    handle,
                    capture_tick,
                    state: PublicationState::Pending { candidate },
                });
                self.last_refusal = None;
            }
            Err(refusal) => {
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
        if let Some(tracked) = self.publication.as_mut() {
            tracked.state = state;
        }
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

    /// THE graceful durable close (behavior *E*): the final required
    /// snapshot with readback verification, the SQLite WAL TRUNCATE
    /// lifecycle checkpoint, the settled required confirmations, the
    /// clean worker join — `failure == None` is the only complete
    /// success (the accepted `CloseReport` contract, surfaced
    /// truthfully).
    pub fn close(self) -> CloseReport {
        self.session.close()
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

/// Runs the integrated graphical demonstration (behavior *B–E* over
/// the one authoritative persisted session). The loop mirrors the
/// Stage 4 `--render --regions` skeleton exactly — the only
/// differences are the orchestrated driver behind it, the `Space`
/// action, the campaign overlay block, and the graceful durable close
/// on exit (run before any validation-error report, so a rendering
/// failure never leaves the campaign unclosed).
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

    // The campaign: found-and-anchor, or recover (fail closed on
    // corrupt/divergent/unverifiable state — never an overwrite).
    let timing = SimConfig::default();
    let (mut demo, launch) = PersistDemo::open_or_recover(&options.campaign_dir, timing)?;

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
             cycle) · O overlay · Esc/close quit (runs the graceful durable close)"
        );
    }

    let mut camera = Camera {
        position: [100.0, 2.0, 9.0],
        ..Camera::default()
    };
    let mut cpu_work_ms: Vec<f64> = Vec::new();
    let mut frame_intervals_ms: Vec<f64> = Vec::new();
    let mut presented: u64 = 0;
    let mut action_completed_printed = false;
    let start = Instant::now();
    let frame_interval = if options.fps == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(
            1.0 / f64::from(options.fps.clamp(1, 1000)),
        ))
    };
    let mut frame_timing = FrameTiming::new();

    loop {
        let frame_start = Instant::now();
        let snapshot = app.frame();
        if snapshot.quit_requested || snapshot.keys_pressed.contains(&Keycode::Escape) {
            break;
        }
        if snapshot.keys_pressed.contains(&Keycode::O) {
            renderer.set_overlay_enabled(!renderer.overlay_enabled());
        }
        // THE deliberate action: one keypress → the region's full
        // durable consequence cycle (behavior *C*). A refusal (the
        // worker down) is reported and the run continues — the next
        // tick fails closed if the gate is genuinely down.
        if snapshot.keys_pressed.contains(&Keycode::Space) {
            match demo.excursion() {
                Ok(record) => println!("action started: {}", record.describe()),
                Err(message) => println!("action refused: {message}"),
            }
        }

        let split = frame_timing.begin_frame(frame_start, !snapshot.minimized);
        frame_intervals_ms.push((split.sim_advance.as_secs_f64() * 1000.0).min(100.0));
        let len = frame_intervals_ms.len();
        if len > 600 {
            frame_intervals_ms.drain(0..len - 600);
        }

        if snapshot.minimized {
            // The campaign clock keeps advancing while the application
            // runs — no silent pause policy (the accepted discipline).
            demo.advance(split.sim_advance)
                .map_err(|error| format!("the authoritative tick refused: {error:?}"))?;
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
        demo.advance(split.sim_advance)
            .map_err(|error| format!("the authoritative tick refused: {error:?}"))?;
        demo.poll();

        if demo
            .action()
            .is_some_and(|record| record.credits_after.is_some())
            && !action_completed_printed
        {
            action_completed_printed = true;
            if let Some(record) = demo.action() {
                println!("action completed: {}", record.describe());
            }
        }

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

        let frame_result = renderer
            .render_frame(
                &view_proj,
                &region_scene,
                overlay_text.as_deref(),
                app.window_size(),
            )
            .map_err(|error| format!("{error}"))?;

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
        println!(
            "sim time: advanced {:.1} ms · ticks due {} = campaign {} + outstanding {} (accounted, never discarded)",
            frame_timing.total_sim_advanced().as_secs_f64() * 1000.0,
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

    // THE GRACEFUL DURABLE CLOSE (behavior *E*) — always runs, even
    // when a validation error is about to fail the run: durability is
    // independent of rendering, and the campaign must never be left
    // unclosed by a rendering-correctness failure.
    println!("--- campaign close ---");
    let report = demo.close();
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
        None => {
            println!(
                "  close: COMPLETE — verified final snapshot + lifecycle checkpoint + settled confirmations + clean join"
            );
            println!(
                "relaunch with the same --demo-persist directory to recover this campaign \
                 (no input replay, no duplicated consequences)"
            );
        }
        Some(failure) => {
            return Err(format!(
                "the close did not complete: {failure:?} — the verified evidence above stands; \
                 the campaign remains recoverable from its durable state"
            ));
        }
    }
    for message in &stats.recent_validation {
        println!("  validation: {message}");
    }
    if stats.validation_active && stats.validation_errors > 0 {
        return Err(format!(
            "validation layer reported {} error(s) while active — see the messages above",
            stats.validation_errors
        ));
    }
    println!(
        "note: instrument readings of this demonstration scene — not \
         first-game performance claims; software-ICD runs are correctness \
         smoke only, and physical graphical acceptance belongs to the \
         owner's rig."
    );
    Ok(())
}

/// Runs the demonstration headlessly: the same campaign lifecycle,
/// tick discipline, action, publication, and close over the fixed
/// [`DEMO_PRESENCE`] (the deliberate action's excursion overrides it
/// while active) — the deterministic evidence path (no GPU, no
/// window; graphical verification belongs to physical hardware).
pub fn run_persist_demo_headless(options: DemoOptions) -> Result<HeadlessReport, String> {
    let timing = SimConfig::default();
    let (mut demo, launch) = PersistDemo::open_or_recover(&options.campaign_dir, timing)?;
    for tick_index in 0..options.headless_ticks {
        if options.action_at == Some(tick_index) {
            let record = demo.excursion()?;
            println!("action @ tick {tick_index}: {}", record.describe());
        }
        demo.run_headless(1)
            .map_err(|error| format!("tick {tick_index} refused: {error:?}"))?;
    }
    demo.wait_current();
    let action = demo.action().cloned();
    let action_publication = demo.publication_state().cloned();
    let closed_at_tick = demo.session().clock();
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
    let report = demo.close();
    Ok(HeadlessReport {
        launch,
        action,
        action_publication,
        closed_at_tick,
        closed_fingerprint,
        closed_region,
        close: CloseSummary::of(&report),
    })
}
