//! STAGE 6 S6-C3 — the versioned, logical-tick-indexed integrated
//! soak scenario (`--scenario`), its strict namespaced CLI options
//! (`--soak-wall-seconds` and companions), and the checkpoint-scale
//! finite-cohort accounting (the owner GO of 2026-10-27).
//!
//! **The contract:**
//!
//! *Tick-indexed inputs.* Every authoritative scenario input — the
//! scripted presence, a burst injection, a deliberate action, an
//! optional publication — is a pure function of the scenario version,
//! the seed, and the LOGICAL TICK NUMBER, applied inside
//! `PersistDemo::run_ticks` per executed tick (never per rendered
//! frame): a catch-up batch of N ticks receives N distinct scripted
//! inputs, so frame cadence, FPS, and catch-up depth cannot change
//! logical behavior. No OS time, host scheduling, filesystem
//! enumeration order, random-device state, or render frame count
//! determines an authoritative input; the seed feeds only the
//! documented deterministic due-offset jitter (so the seed is a real
//! part of the schedule identity, never decoration). The scenario
//! never fabricates NPC routine consequences — burst payloads carry
//! the stage-6 burst tag (foreign to the routine-chain decode,
//! exactly as S6-C2's accepted cohort), and the deliberate action
//! runs through the accepted excursion lifecycle.
//!
//! *Versioned phases.* `stage6-v1` is the 9000-tick integrated stress
//! profile (the phase plan at the demo's 10 Hz cadence ≈ 900 seconds
//! of logical time); `stage6-v1-short` is the compressed
//! CI/determinism fixture — the same nine phases, scaled, with the
//! documented differences (one action cycle; a compressed tour).
//! Logical phase boundaries are TICK windows, never wall-clock
//! timers: whether a phase ran inside the measured wall window is
//! recorded as honest OBSERVED coverage, kept strictly separate from
//! the logical schedule. A phase executed during post-soak catch-up
//! is never evidence that it ran inside the measured window.
//!
//! *Checkpoint-scale accounting.* The S6-C2 per-identity oracle is
//! O(ledger)+O(queue) — correct at controlled checkpoints and at
//! finalization, never per tick. Every checkpoint verifies the O(1)
//! whole-campaign conservation identity with the spine's own
//! definitions (the living NPC campaign's recurring obligations are
//! part of the accounted state and are never required to drain); the
//! per-cohort live states come from single queue/held passes; the
//! full identity-level zero-silent-drop verdict (executed
//! exactly-once, duplicate detection, missing detection) is
//! established at finalization through the FULL ledger when
//! `HistoryKind::Full` holds (the demo scenarios never compact), and
//! honestly by subtraction plus the live sets if a future policy
//! compacts — never silently claimed as full-history evidence.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use engine_core::{EventId, Field, HistoryKind, ObligationLocation, PresenceSample};

use crate::npc::REGION_COUNT;
use crate::orchestration::{OrchestratedSession, ScheduleOpError};
use crate::region::{PresenceScenario, stage4_state_fingerprint_hex};

/// The stage-6 burst payload tag (identical semantics to the accepted
/// S6-C2 cohort): distinct from the routine-chain tag `ROUTINE_MAGIC`
/// (`0x5E47`), so a burst payload can never decode as an NPC routine
/// chain — counted in the spine's ledger, never advancing NPC state.
pub const BURST_MAGIC: u64 = 0x5336;

/// The canonical seed of the stage-6 scenario family (the documented
/// default; `--scenario-seed` overrides it, and the seed is recorded
/// in every report and evidence manifest).
pub const STAGE6_SCENARIO_SEED: u64 = 0x5354_4706;

/// The upper bound of `--soak-wall-seconds` — an explicit rejection
/// never a silent clamp (the formal gate is 900).
pub const MAX_SOAK_WALL_SECONDS: u64 = 86_400;

/// The default post-soak stabilization deadline (seconds): the
/// bounded window in which accrued fixed-step debt must drain to zero
/// before the workload gate is marked FAILED/INCOMPLETE.
pub const DEFAULT_STABILIZATION_DEADLINE_SECS: u64 = 300;

/// The number of phases in every stage-6 scenario profile.
pub const PHASE_COUNT: usize = 9;

/// One logical phase window (scenario-relative ticks, `start`
/// inclusive and `end` exclusive — tick windows, never wall-clock
/// timers).
#[derive(Debug, Clone, Copy)]
pub struct PhasePlan {
    pub start: u64,
    pub end: u64,
    pub name: &'static str,
}

/// The `stage6-v1` profile: 9000 logical ticks ≈ 900 seconds at the
/// demo's 10 Hz cadence under real-time progress (the §5 phase plan).
const STAGE6_V1_PHASES: [PhasePlan; PHASE_COUNT] = [
    PhasePlan {
        start: 0,
        end: 1200,
        name: "baseline",
    },
    PhasePlan {
        start: 1200,
        end: 1800,
        name: "clustered-bursts",
    },
    PhasePlan {
        start: 1800,
        end: 3000,
        name: "backlog-drain",
    },
    PhasePlan {
        start: 3000,
        end: 3300,
        name: "action-cycle",
    },
    PhasePlan {
        start: 3300,
        end: 4200,
        name: "publication-pressure",
    },
    PhasePlan {
        start: 4200,
        end: 5400,
        name: "sustained-overload",
    },
    PhasePlan {
        start: 5400,
        end: 6600,
        name: "overload-drain",
    },
    PhasePlan {
        start: 6600,
        end: 7800,
        name: "lod-tour",
    },
    PhasePlan {
        start: 7800,
        end: 9000,
        name: "stabilization",
    },
];

/// The compressed CI/determinism fixture: the same nine phases over
/// 300 logical ticks. Documented differences from the full profile
/// (never silently equivalent): the injection rates are scaled, the
/// deliberate-action cycle runs ONCE (phase 3), and the tour is
/// compressed (no second action) — the full profile runs the action
/// cycle twice and a 150-ticks-per-region tour.
const STAGE6_V1_SHORT_PHASES: [PhasePlan; PHASE_COUNT] = [
    PhasePlan {
        start: 0,
        end: 60,
        name: "baseline",
    },
    PhasePlan {
        start: 60,
        end: 80,
        name: "clustered-bursts",
    },
    PhasePlan {
        start: 80,
        end: 110,
        name: "backlog-drain",
    },
    PhasePlan {
        start: 110,
        end: 125,
        name: "action-cycle",
    },
    PhasePlan {
        start: 125,
        end: 150,
        name: "publication-pressure",
    },
    PhasePlan {
        start: 150,
        end: 190,
        name: "sustained-overload",
    },
    PhasePlan {
        start: 190,
        end: 230,
        name: "overload-drain",
    },
    PhasePlan {
        start: 230,
        end: 266,
        name: "lod-tour",
    },
    PhasePlan {
        start: 266,
        end: 300,
        name: "stabilization",
    },
];

/// The controlled checkpoint boundaries (scenario-relative ticks,
/// AFTER the boundary tick executes): the in-run light accounting —
/// the O(1) conservation identity plus the per-cohort live states —
/// never the full ledger scan (that belongs to finalization and the
/// headless determinism replays).
const STAGE6_V1_CHECKPOINTS: [u64; 10] =
    [600, 1200, 1800, 3000, 3300, 4200, 5400, 6600, 6700, 7800];
const STAGE6_V1_SHORT_CHECKPOINTS: [u64; 8] = [60, 110, 150, 190, 230, 234, 240, 266];

/// The burst cohort roles (phase-tagged; the registry keys on
/// `(cohort, index)`).
pub const CLUSTER_COHORT: u16 = 1;
pub const STREAM_COHORT: u16 = 2;
pub const DRAIN_COHORT: u16 = 3;
pub const OVERLOAD_COHORT: u16 = 5;
pub const HELD_COHORT: u16 = 7;

/// The versioned scenario identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoakScenarioId {
    /// The 9000-tick integrated stress profile (the formal soak
    /// workload; ≈ 900 seconds of logical time at 10 Hz).
    Stage6V1,
    /// The compressed 300-tick CI/determinism fixture (all nine
    /// phases, scaled, with documented differences).
    Stage6V1Short,
}

impl SoakScenarioId {
    /// The CLI identifier (also the evidence scenario label).
    pub fn as_str(self) -> &'static str {
        match self {
            SoakScenarioId::Stage6V1 => "stage6-v1",
            SoakScenarioId::Stage6V1Short => "stage6-v1-short",
        }
    }

    /// Parses the CLI value; an unknown identifier is a visible
    /// error listing the known versions (never a silent default).
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "stage6-v1" => Ok(SoakScenarioId::Stage6V1),
            "stage6-v1-short" => Ok(SoakScenarioId::Stage6V1Short),
            _ => Err(format!(
                "unknown soak scenario \"{text}\" (known: stage6-v1, stage6-v1-short)"
            )),
        }
    }

    /// The scenario's logical horizon (total scripted ticks).
    pub fn horizon(self) -> u64 {
        match self {
            SoakScenarioId::Stage6V1 => 9000,
            SoakScenarioId::Stage6V1Short => 300,
        }
    }

    /// The phase windows, in order.
    pub fn phases(self) -> &'static [PhasePlan] {
        match self {
            SoakScenarioId::Stage6V1 => &STAGE6_V1_PHASES,
            SoakScenarioId::Stage6V1Short => &STAGE6_V1_SHORT_PHASES,
        }
    }

    /// The controlled checkpoint boundaries.
    pub fn checkpoints(self) -> &'static [u64] {
        match self {
            SoakScenarioId::Stage6V1 => &STAGE6_V1_CHECKPOINTS,
            SoakScenarioId::Stage6V1Short => &STAGE6_V1_SHORT_CHECKPOINTS,
        }
    }

    /// Whether `tick` (the just-executed scenario tick) is a
    /// controlled checkpoint boundary.
    pub fn is_checkpoint(self, tick: u64) -> bool {
        self.checkpoints().contains(&tick)
    }
}

/// Encodes one cohort member's payload: `magic(16) | cohort(16) |
/// index(32)` (the accepted S6-C2 encoding, re-stated for the
/// application-layer driver).
pub fn burst_payload(cohort: u16, index: u32) -> u64 {
    (BURST_MAGIC << 48) | (u64::from(cohort) << 32) | u64::from(index)
}

/// The deterministic seeded jitter (SplitMix64 over the seed, the
/// scenario tick, and a salt): the ONLY seed-dependent input, a small
/// due-offset spread that keeps the schedule identity honestly
/// seed-sensitive without touching phase structure.
fn seeded_jitter(seed: u64, tick: u64, salt: u64, span: u64) -> u64 {
    if span == 0 {
        return 0;
    }
    let mut z =
        seed ^ tick.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ salt.wrapping_mul(0x6189_5d68_2d7f_9e1b);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (z ^ (z >> 31)) % span
}

/// One scripted burst member (the descriptor; the caller —
/// `PersistDemo::run_ticks` — constructs the `EventSpec` through the
/// accepted `OrchestratedSession::schedule_event` boundary with
/// `due = the upcoming campaign tick + due_offset`, so scheduling is
/// never a late arrival by construction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstInput {
    pub cohort: u16,
    pub index: u32,
    pub field: Field,
    pub region: u16,
    pub due_offset: u64,
}

/// The scripted inputs for ONE logical tick (deterministically
/// ordered: presence first, then the bursts in emission order, then
/// the action, then the publication — the application applies them in
/// exactly this order).
#[derive(Debug, Clone)]
pub struct ScenarioStep {
    pub scenario_tick: u64,
    pub presence: PresenceSample,
    pub bursts: Vec<BurstInput>,
    pub fire_action: bool,
    pub fire_publication: bool,
}

/// One accepted cohort member's registry entry (built ONLY from
/// `schedule_event`'s actual return value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registered {
    pub id: EventId,
    pub cohort: u16,
    pub index: u32,
}

/// One rejected scheduling attempt (recorded separately, never
/// counted as an accepted identity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub cohort: u16,
    pub index: u32,
    pub reason: String,
}

/// One scripted action attempt's observed outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionObservation {
    pub scenario_tick: u64,
    pub accepted: bool,
    pub detail: String,
}

/// One scripted optional-publication attempt (the outcome lands in
/// the accepted persistence-evidence families — capture, admission,
/// busy, completion — and the close's completions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicationObservation {
    pub scenario_tick: u64,
}

/// The per-cohort accounting verdict (the S6-C2 oracle's shape,
/// checkpoint-scale).
#[derive(Debug, Clone, PartialEq)]
pub struct CohortAccounting {
    pub cohort: u16,
    pub expected: usize,
    pub executed: usize,
    pub duplicate_executions: usize,
    pub queued: usize,
    pub blocked: usize,
    pub held: usize,
    pub missing: usize,
    /// A bounded sample of missing identities (the silent-drop
    /// signal's inspectable evidence; the count is authoritative).
    pub missing_samples: Vec<EventId>,
    pub late_count: usize,
    pub max_lateness_ticks: u64,
    /// `true` when `executed` was derived by subtraction (compacted
    /// history) rather than counted per identity in the full ledger —
    /// the honest qualification, never silently equivalent.
    pub by_subtraction: bool,
}

impl CohortAccounting {
    /// No accepted identity is in NO state, none executed twice, and
    /// every identity is accounted in exactly the spine's states.
    pub fn fully_accounted(&self) -> bool {
        self.missing == 0
            && self.duplicate_executions == 0
            && self.executed + self.queued + self.blocked + self.held == self.expected
    }

    /// Fully accounted AND drained: the finite cohort's terminal
    /// state.
    pub fn complete(&self) -> bool {
        self.fully_accounted() && self.queued + self.blocked + self.held == 0
    }

    /// The identity-evidence qualification's short label.
    pub fn identity_note(&self) -> &'static str {
        if self.by_subtraction {
            "by-subtraction"
        } else {
            "full-ledger"
        }
    }
}

/// One checkpoint's observations (logical state only — wall-clock
/// coverage is recorded separately and never conflated with it).
#[derive(Debug, Clone)]
pub struct ScenarioCheckpoint {
    pub scenario_tick: u64,
    pub campaign_tick: u64,
    /// The app-owned fixed-step time debt at the boundary (a separate
    /// quantity from event-spine obligations, never restored by
    /// recovery).
    pub fixed_step_debt: u32,
    pub conservation: bool,
    pub scheduled: u64,
    pub executed: u64,
    pub queued: u64,
    pub blocked: u64,
    pub held: u64,
    pub processed_through: Option<u64>,
    pub max_executed_due: Option<u64>,
    pub history_full: bool,
    /// The campaign fingerprint at the boundary (`stage4-state/1`
    /// when the history is full; `None` with the honest note when a
    /// compacted instance cannot compare through the same domain).
    pub fingerprint: Option<String>,
    /// "full-ledger" (per-identity evidence) or "by-subtraction"
    /// (compacted-history qualification).
    pub identity_evidence: &'static str,
    pub cohorts: Vec<CohortAccounting>,
}

/// One phase's OBSERVED coverage (wall stamps are milliseconds since
/// the measurement origin — `None` before the measurement began or
/// when the phase never ran).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseObservation {
    pub index: usize,
    pub name: &'static str,
    pub start: u64,
    pub end: u64,
    pub entered: bool,
    pub wall_offset_ms: Option<f64>,
}

/// The measured-window coverage report (strictly observational).
#[derive(Debug, Clone)]
pub struct CoverageReport {
    pub phases: Vec<PhaseObservation>,
    /// The scenario's logical horizon was reached (every scripted
    /// input consumed).
    pub horizon_reached: bool,
}

impl CoverageReport {
    /// Every phase of the profile ran.
    pub fn all_phases_entered(&self) -> bool {
        self.phases.iter().all(|phase| phase.entered)
    }
}

/// The scenario's end-of-run report (application-level — the evidence
/// package stays the pure s6-evidence/2 five-stream contract; scenario
/// outcomes never enter the evidence schema).
#[derive(Debug, Clone)]
pub struct ScenarioRunReport {
    pub scenario: &'static str,
    pub seed: u64,
    pub scenario_tick: u64,
    pub horizon: u64,
    pub registered: usize,
    pub rejected: usize,
    pub coverage: CoverageReport,
    pub checkpoints: Vec<ScenarioCheckpoint>,
    pub terminal: ScenarioCheckpoint,
    pub actions: Vec<ActionObservation>,
    pub publications: usize,
}

impl ScenarioRunReport {
    /// The hard scenario gates (never the NPC campaign draining): the
    /// whole-campaign conservation identity at EVERY checkpoint and at
    /// the terminal boundary, and the zero-silent-drop verdict — no
    /// missing identity, no duplicate execution, every accepted
    /// identity accounted in exactly the spine's states.
    pub fn verdict_ok(&self) -> bool {
        self.checkpoints
            .iter()
            .all(|checkpoint| checkpoint.conservation)
            && self.terminal.conservation
            && self.terminal.cohorts.iter().all(|cohort| {
                cohort.missing == 0
                    && cohort.duplicate_executions == 0
                    && cohort.executed + cohort.queued + cohort.blocked + cohort.held
                        == cohort.expected
            })
    }

    /// The finite cohorts' terminal drain status (outstanding work is
    /// reported honestly, never silently required to reach zero).
    pub fn cohorts_drained(&self) -> bool {
        self.terminal
            .cohorts
            .iter()
            .all(|cohort| cohort.queued + cohort.blocked + cohort.held == 0)
    }
}

/// The tick-indexed scenario driver: pure scripted inputs plus the
/// finite registries. Held by `PersistDemo` (opt-in); one `step()`
/// per EXECUTED logical tick.
pub struct SoakScenario {
    id: SoakScenarioId,
    seed: u64,
    centers: [(f64, f64); REGION_COUNT],
    scenario_tick: u64,
    next_index: HashMap<u16, u32>,
    registered: Vec<Registered>,
    rejected: Vec<Rejected>,
    actions_fired: Vec<ActionObservation>,
    publications_fired: Vec<PublicationObservation>,
    phase_entered: [bool; PHASE_COUNT],
    phase_wall_ms: [Option<f64>; PHASE_COUNT],
    measurement_origin: Option<Instant>,
    /// The in-run light checkpoints (recorded at the controlled
    /// boundaries by the application loop).
    checkpoints: Vec<ScenarioCheckpoint>,
}

impl SoakScenario {
    /// Builds the driver; `centers` comes from the live
    /// `RegionDirector`'s config (the tour and stationary targets are
    /// expressed against the actual geometry).
    pub fn new(id: SoakScenarioId, seed: u64, centers: [(f64, f64); REGION_COUNT]) -> Self {
        SoakScenario {
            id,
            seed,
            centers,
            scenario_tick: 0,
            next_index: HashMap::new(),
            registered: Vec::new(),
            rejected: Vec::new(),
            actions_fired: Vec::new(),
            publications_fired: Vec::new(),
            phase_entered: [false; PHASE_COUNT],
            phase_wall_ms: [None; PHASE_COUNT],
            measurement_origin: None,
            checkpoints: Vec::new(),
        }
    }

    /// The versioned identity.
    pub fn id(&self) -> SoakScenarioId {
        self.id
    }

    /// The effective seed (part of the schedule identity).
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The next scenario tick to be scripted (the count of steps
    /// already consumed).
    pub fn scenario_tick(&self) -> u64 {
        self.scenario_tick
    }

    /// The registry of accepted identities.
    pub fn registered(&self) -> &[Registered] {
        &self.registered
    }

    /// The recorded rejections (never counted as accepted).
    pub fn rejected(&self) -> &[Rejected] {
        &self.rejected
    }

    /// The scripted action attempts and their observed outcomes.
    pub fn actions_fired(&self) -> &[ActionObservation] {
        &self.actions_fired
    }

    /// The scripted optional-publication attempts.
    pub fn publications_fired(&self) -> &[PublicationObservation] {
        &self.publications_fired
    }

    /// Sets the measurement origin (the S6-C3 runner, at readiness):
    /// phase-entry wall stamps are recorded as offsets from this
    /// instant; phases entered before it stamp `None` (honestly
    /// "before the measured window").
    pub fn set_measurement_origin(&mut self, origin: Instant) {
        self.measurement_origin = Some(origin);
    }

    /// Produces the scripted inputs for the NEXT logical tick. The
    /// step is a pure function of (version, seed, tick) — the only
    /// mutable state is the per-cohort index allocator and the
    /// observational phase stamps, both deterministic given the tick
    /// sequence.
    pub fn step(&mut self) -> ScenarioStep {
        let tick = self.scenario_tick;
        self.scenario_tick += 1;
        let phase = self.phase_index_for(tick);
        if !self.phase_entered[phase] {
            self.phase_entered[phase] = true;
            self.phase_wall_ms[phase] = self
                .measurement_origin
                .map(|origin| origin.elapsed().as_secs_f64() * 1000.0);
        }
        ScenarioStep {
            scenario_tick: tick,
            presence: self.presence_at(tick),
            bursts: self.bursts_at(tick),
            fire_action: self.action_at(tick),
            fire_publication: self.publication_at(tick),
        }
    }

    /// Records one scheduling attempt's actual outcome (the registry
    /// is built ONLY from the scheduler's own returns).
    pub fn record_scheduled(
        &mut self,
        cohort: u16,
        index: u32,
        result: Result<EventId, ScheduleOpError>,
    ) {
        match result {
            Ok(id) => self.registered.push(Registered { id, cohort, index }),
            Err(error) => self.rejected.push(Rejected {
                cohort,
                index,
                reason: format!("{error:?}"),
            }),
        }
    }

    /// Records one scripted action attempt's observed outcome.
    pub fn record_action(&mut self, scenario_tick: u64, accepted: bool, detail: String) {
        self.actions_fired.push(ActionObservation {
            scenario_tick,
            accepted,
            detail,
        });
    }

    /// Records one scripted optional-publication attempt.
    pub fn record_publication(&mut self, scenario_tick: u64) {
        self.publications_fired
            .push(PublicationObservation { scenario_tick });
    }

    /// Records one in-run light checkpoint (computed by the
    /// application loop at the controlled boundary).
    pub fn record_checkpoint(&mut self, checkpoint: ScenarioCheckpoint) {
        self.checkpoints.push(checkpoint);
    }

    /// Assembles the end-of-run report around the terminal
    /// accounting (computed by the caller while the session still
    /// lives).
    pub fn run_report(&self, terminal: ScenarioCheckpoint) -> ScenarioRunReport {
        ScenarioRunReport {
            scenario: self.id.as_str(),
            seed: self.seed,
            scenario_tick: self.scenario_tick,
            horizon: self.id.horizon(),
            registered: self.registered.len(),
            rejected: self.rejected.len(),
            coverage: self.coverage(),
            checkpoints: self.checkpoints.clone(),
            terminal,
            actions: self.actions_fired.clone(),
            publications: self.publications_fired.len(),
        }
    }

    /// The OBSERVED coverage at any moment (logical entry flags plus
    /// wall stamps relative to the measurement origin).
    pub fn coverage(&self) -> CoverageReport {
        CoverageReport {
            phases: self
                .id
                .phases()
                .iter()
                .enumerate()
                .map(|(index, phase)| PhaseObservation {
                    index,
                    name: phase.name,
                    start: phase.start,
                    end: phase.end,
                    entered: self.phase_entered[index],
                    wall_offset_ms: self.phase_wall_ms[index],
                })
                .collect(),
            horizon_reached: self.scenario_tick >= self.id.horizon(),
        }
    }

    /// The checkpoint-scale accounting (§8): the O(1) whole-campaign
    /// conservation identity with the spine's own definitions, the
    /// spine totals, the watermark pair, the boundary fingerprint,
    /// and the per-cohort live states — plus, when `full` is set and
    /// the history is intact, the identity-level zero-silent-drop
    /// verdict through the full ledger. A compacted history degrades
    /// honestly to by-subtraction (never silently claimed as
    /// full-history evidence).
    pub fn checkpoint(
        &self,
        session: &OrchestratedSession,
        fixed_step_debt: u32,
        full: bool,
    ) -> ScenarioCheckpoint {
        let spine = session.spine();
        let stats = spine.stats();
        let queued_total = spine.pending_count() as u64;
        let blocked_total = spine.blocked_count() as u64;
        let held_total = spine.held_count() as u64;
        let conservation =
            stats.scheduled == stats.executed + queued_total + blocked_total + held_total;
        let history_full = matches!(spine.history_kind(), HistoryKind::Full);
        // The per-cohort live states: ONE pass over the queues and
        // the held view, bucketed by the registry's identity→cohort
        // map (never a per-cohort rescan).
        let cohort_of: HashMap<EventId, u16> = self
            .registered
            .iter()
            .map(|entry| (entry.id, entry.cohort))
            .collect();
        let mut live: HashMap<u16, [usize; 3]> = HashMap::new();
        let mut outstanding_ids: HashSet<EventId> = HashSet::new();
        for view in spine.collect_matching(|view| cohort_of.contains_key(&view.id)) {
            outstanding_ids.insert(view.id);
            let bucket = live.entry(cohort_of[&view.id]).or_insert([0, 0, 0]);
            match view.location {
                ObligationLocation::Queued => bucket[0] += 1,
                ObligationLocation::Blocked => bucket[1] += 1,
                ObligationLocation::Held => bucket[2] += 1,
            }
        }
        for view in spine.held_view() {
            if let Some(cohort) = cohort_of.get(&view.id) {
                outstanding_ids.insert(view.id);
                live.entry(*cohort).or_insert([0, 0, 0])[2] += 1;
            }
        }
        // The identity-level execution oracle — only at the
        // controlled full checkpoints, and only when the full ledger
        // actually exists.
        let mut executed_per_cohort: HashMap<u16, HashSet<EventId>> = HashMap::new();
        let mut duplicates = HashMap::<u16, usize>::new();
        let mut late_counts = HashMap::<u16, usize>::new();
        let mut max_lateness = HashMap::<u16, u64>::new();
        let identity_evidence = if full && history_full {
            let ledger = spine
                .try_ledger_full()
                .expect("the full history (asserted by history_kind)");
            for record in ledger.iter() {
                let Some(cohort) = cohort_of.get(&record.id) else {
                    continue;
                };
                if !executed_per_cohort
                    .entry(*cohort)
                    .or_default()
                    .insert(record.id)
                {
                    *duplicates.entry(*cohort).or_insert(0) += 1;
                }
                if record.late {
                    *late_counts.entry(*cohort).or_insert(0) += 1;
                    let lateness = record.executed_tick.as_u64() - record.intended_due.as_u64();
                    max_lateness.insert(
                        *cohort,
                        max_lateness.get(cohort).copied().unwrap_or(0).max(lateness),
                    );
                }
            }
            "full-ledger"
        } else {
            "by-subtraction"
        };
        let mut cohorts: Vec<CohortAccounting> = Vec::new();
        let mut known: HashMap<u16, usize> = HashMap::new();
        for entry in &self.registered {
            *known.entry(entry.cohort).or_insert(0) += 1;
        }
        for (cohort, expected) in known {
            let bucket = live.get(&cohort).copied().unwrap_or([0, 0, 0]);
            let (executed, by_subtraction, missing, missing_samples) = if identity_evidence
                == "full-ledger"
            {
                let executed_set = executed_per_cohort.get(&cohort);
                let mut missing_ids = Vec::new();
                for entry in self.registered.iter().filter(|e| e.cohort == cohort) {
                    let executed = executed_set.is_some_and(|set| set.contains(&entry.id));
                    if !executed && !outstanding_ids.contains(&entry.id) && missing_ids.len() < 8 {
                        missing_ids.push(entry.id);
                    }
                }
                (
                    executed_set.map_or(0, HashSet::len),
                    false,
                    expected.saturating_sub(
                        executed_set.map_or(0, HashSet::len) + bucket[0] + bucket[1] + bucket[2],
                    ),
                    missing_ids,
                )
            } else {
                // Compacted history: executed by subtraction (the
                // conservation identity and the live sets still
                // bound every identity; per-identity duplicate
                // detection for the compacted range is honestly
                // unavailable).
                (
                    expected.saturating_sub(bucket[0] + bucket[1] + bucket[2]),
                    true,
                    0,
                    Vec::new(),
                )
            };
            cohorts.push(CohortAccounting {
                cohort,
                expected,
                executed,
                duplicate_executions: duplicates.get(&cohort).copied().unwrap_or(0),
                queued: bucket[0],
                blocked: bucket[1],
                held: bucket[2],
                missing,
                missing_samples,
                late_count: late_counts.get(&cohort).copied().unwrap_or(0),
                max_lateness_ticks: max_lateness.get(&cohort).copied().unwrap_or(0),
                by_subtraction,
            });
        }
        cohorts.sort_by_key(|accounting| accounting.cohort);
        ScenarioCheckpoint {
            scenario_tick: self.scenario_tick,
            campaign_tick: session.clock().as_u64(),
            fixed_step_debt,
            conservation,
            scheduled: stats.scheduled,
            executed: stats.executed,
            queued: queued_total,
            blocked: blocked_total,
            held: held_total,
            processed_through: spine.processed_through().map(|tick| tick.as_u64()),
            max_executed_due: spine.max_executed_due().map(|tick| tick.as_u64()),
            history_full,
            fingerprint: if history_full {
                Some(stage4_state_fingerprint_hex(spine, session.director()))
            } else {
                None
            },
            identity_evidence,
            cohorts,
        }
    }

    /// The terminal accounting (finalization): the full identity
    /// oracle over the whole registry.
    pub fn terminal_accounting(&self, session: &OrchestratedSession) -> ScenarioCheckpoint {
        self.checkpoint(session, 0, true)
    }

    fn phase_index_for(&self, tick: u64) -> usize {
        let phases = self.id.phases();
        for (index, phase) in phases.iter().enumerate() {
            if tick >= phase.start && tick < phase.end {
                return index;
            }
        }
        // Past the horizon (the total-function tail): the final
        // phase's observational slot.
        PHASE_COUNT - 1
    }

    fn presence_at(&self, tick: u64) -> PresenceSample {
        match self.id {
            SoakScenarioId::Stage6V1 => {
                if tick < 600 {
                    PresenceScenario::Stationary {
                        position: crate::persist_demo::DEMO_PRESENCE.position,
                    }
                    .sample_at(tick, &self.centers)
                } else if tick < 1200 {
                    PresenceScenario::Walk {
                        from: (100.0, 9.0),
                        to: (-100.0, 9.0),
                        ticks: 600,
                    }
                    .sample_at(tick - 600, &self.centers)
                } else if tick < 6600 {
                    // The far plate: region 4 stays Full through the
                    // burst, action, publication, and overload phases
                    // (its citizens accrue the consequential signals
                    // the action cycle later captures).
                    PresenceScenario::Stationary {
                        position: (-100.0, 9.0),
                    }
                    .sample_at(tick, &self.centers)
                } else if tick < 7800 {
                    PresenceScenario::Tour {
                        ticks_per_region: 150,
                    }
                    .sample_at(tick - 6600, &self.centers)
                } else {
                    // The stabilization presence: the tour's final
                    // position (region 7's plate) held to the end —
                    // and past the horizon (the total-function tail).
                    PresenceScenario::Stationary {
                        position: self.centers[7],
                    }
                    .sample_at(tick, &self.centers)
                }
            }
            SoakScenarioId::Stage6V1Short => {
                if tick < 230 {
                    PresenceScenario::Stationary {
                        position: crate::persist_demo::DEMO_PRESENCE.position,
                    }
                    .sample_at(tick, &self.centers)
                } else if tick < 266 {
                    PresenceScenario::Tour {
                        ticks_per_region: 4,
                    }
                    .sample_at(tick - 230, &self.centers)
                } else {
                    PresenceScenario::Stationary {
                        position: self.centers[0],
                    }
                    .sample_at(tick, &self.centers)
                }
            }
        }
    }

    fn bursts_at(&mut self, tick: u64) -> Vec<BurstInput> {
        let mut bursts = Vec::new();
        let seed = self.seed;
        match self.id {
            SoakScenarioId::Stage6V1 => {
                if tick == 1200 {
                    // The historically relevant 200-event synchronized
                    // cluster: one shared (seeded) due offset — every
                    // member due the SAME tick, near/far split even.
                    let offset = 20 + seeded_jitter(seed, tick, 1, 8);
                    self.emit(&mut bursts, CLUSTER_COHORT, 100, Field::Near, 4, offset);
                    self.emit(&mut bursts, CLUSTER_COHORT, 100, Field::Far, 3, offset);
                } else if tick > 1200 && tick < 1800 && (tick - 1200).is_multiple_of(60) {
                    let offset = 5 + seeded_jitter(seed, tick, 2, 4);
                    self.emit(&mut bursts, STREAM_COHORT, 30, Field::Near, 4, offset);
                    self.emit(&mut bursts, STREAM_COHORT, 30, Field::Far, 3, offset);
                } else if tick > 1800 && tick < 3000 && (tick - 1800).is_multiple_of(150) {
                    let offset = 30 + seeded_jitter(seed, tick, 3, 16);
                    self.emit(&mut bursts, DRAIN_COHORT, 20, Field::Near, 4, offset);
                    self.emit(&mut bursts, DRAIN_COHORT, 20, Field::Far, 3, offset);
                } else if (4200..5400).contains(&tick) {
                    // The sustained finite overload: every tick,
                    // ABOVE the total per-tick budget — the routine
                    // chains self-throttle (a deferred follow-up is
                    // never scheduled), the lanes saturate, and the
                    // finite cohort's backlog grows measurably and
                    // bounded before the termination phases drain it.
                    let offset = 2 + seeded_jitter(seed, tick, 5, 3);
                    self.emit(&mut bursts, OVERLOAD_COHORT, 120, Field::Near, 4, offset);
                    self.emit(&mut bursts, OVERLOAD_COHORT, 120, Field::Far, 3, offset);
                } else if tick == 6650 {
                    // The dormant-hold cohort: injected while the tour
                    // stands at region 0, targeting region 3 (dormant
                    // at that moment) — the held path; the tour's
                    // arrival releases it through the reactivation.
                    let offset = 2 + seeded_jitter(seed, tick, 7, 3);
                    self.emit(&mut bursts, HELD_COHORT, 40, Field::Far, 3, offset);
                }
            }
            SoakScenarioId::Stage6V1Short => {
                if tick == 60 {
                    let offset = 20 + seeded_jitter(seed, tick, 1, 8);
                    self.emit(&mut bursts, CLUSTER_COHORT, 100, Field::Near, 0, offset);
                    self.emit(&mut bursts, CLUSTER_COHORT, 100, Field::Far, 1, offset);
                } else if tick > 60 && tick < 80 && (tick - 60).is_multiple_of(5) {
                    let offset = 5 + seeded_jitter(seed, tick, 2, 4);
                    self.emit(&mut bursts, STREAM_COHORT, 10, Field::Near, 0, offset);
                    self.emit(&mut bursts, STREAM_COHORT, 10, Field::Far, 1, offset);
                } else if tick > 80 && tick < 110 && (tick - 80).is_multiple_of(10) {
                    let offset = 30 + seeded_jitter(seed, tick, 3, 16);
                    self.emit(&mut bursts, DRAIN_COHORT, 10, Field::Near, 0, offset);
                    self.emit(&mut bursts, DRAIN_COHORT, 10, Field::Far, 1, offset);
                } else if (150..190).contains(&tick) {
                    // The compressed overload — above the total
                    // budget, scaled to the fixture's window (the
                    // documented difference from the full profile's
                    // 1200-tick phase).
                    let offset = 2 + seeded_jitter(seed, tick, 5, 3);
                    self.emit(&mut bursts, OVERLOAD_COHORT, 120, Field::Near, 0, offset);
                    self.emit(&mut bursts, OVERLOAD_COHORT, 120, Field::Far, 1, offset);
                } else if tick == 231 {
                    let offset = 2 + seeded_jitter(seed, tick, 7, 3);
                    self.emit(&mut bursts, HELD_COHORT, 20, Field::Far, 4, offset);
                }
            }
        }
        bursts
    }

    /// Allocates `count` fresh scenario identities within the cohort
    /// (the deterministic per-cohort allocator — repeated slices
    /// never reuse a `(cohort, index)` payload).
    fn emit(
        &mut self,
        bursts: &mut Vec<BurstInput>,
        cohort: u16,
        count: u32,
        field: Field,
        region: u16,
        due_offset: u64,
    ) {
        for _ in 0..count {
            let index = *self.next_index.entry(cohort).or_insert(0);
            self.next_index.insert(cohort, index + 1);
            bursts.push(BurstInput {
                cohort,
                index,
                field,
                region,
                due_offset,
            });
        }
    }

    fn action_at(&self, tick: u64) -> bool {
        match self.id {
            // Phase 3 fires on the long-occupied far plate (signals
            // accrued since tick 1200); phase 7 fires 100 ticks into
            // the tour's final region visit, so the excursion's
            // reactivation-gated application completes in-window.
            SoakScenarioId::Stage6V1 => tick == 3050 || tick == 7750,
            // The compressed fixture runs ONE action cycle (the
            // documented difference from the full profile).
            SoakScenarioId::Stage6V1Short => tick == 115,
        }
    }

    fn publication_at(&self, tick: u64) -> bool {
        match self.id {
            SoakScenarioId::Stage6V1 => {
                (3300..4200).contains(&tick) && (tick - 3300).is_multiple_of(30)
            }
            SoakScenarioId::Stage6V1Short => tick == 130 || tick == 140,
        }
    }
}

/// The strict namespaced S6-C3 CLI options (parsed only within the
/// `--demo-persist` mode; every other mode's behavior is unchanged).
/// Every failure is a visible error with a nonzero exit — never a
/// silent default and never a silent clamp.
#[derive(Debug, Clone, Default)]
pub struct SoakOptions {
    /// The measured wall-clock window of the integrated graphical
    /// soak (`--soak-wall-seconds`; the formal gate is 900).
    pub wall_seconds: Option<u64>,
    /// The bounded post-soak stabilization deadline
    /// (`--stabilization-deadline-secs`; default
    /// [`DEFAULT_STABILIZATION_DEADLINE_SECS`]).
    pub stabilization_deadline_secs: Option<u64>,
    /// The versioned scenario (`--scenario`).
    pub scenario: Option<SoakScenarioId>,
    /// The scenario seed override (`--scenario-seed`; default
    /// [`STAGE6_SCENARIO_SEED`]).
    pub scenario_seed: Option<u64>,
    /// The fresh, exclusive evidence directory (`--evidence-dir`).
    pub evidence_dir: Option<PathBuf>,
}

impl SoakOptions {
    /// Scans the argument list for the S6-C3 flags. Per-flag value
    /// validity is checked HERE (missing values, invalid numbers,
    /// unknown scenario identifiers); the relational rules live in
    /// [`SoakOptions::validate`] (they need the mode context only the
    /// caller has).
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = SoakOptions::default();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--soak-wall-seconds" => {
                    let value = args.get(index + 1).ok_or(
                        "--soak-wall-seconds requires a value (the measured wall-clock seconds)",
                    )?;
                    let seconds: u64 = value.parse().map_err(|_| {
                        format!(
                            "--soak-wall-seconds needs a whole number of seconds (got \"{value}\")"
                        )
                    })?;
                    if seconds == 0 || seconds > MAX_SOAK_WALL_SECONDS {
                        return Err(format!(
                            "--soak-wall-seconds must be between 1 and {MAX_SOAK_WALL_SECONDS} \
                             (got {seconds}) — invalid durations are rejected, never clamped"
                        ));
                    }
                    options.wall_seconds = Some(seconds);
                    index += 2;
                }
                "--stabilization-deadline-secs" => {
                    let value = args.get(index + 1).ok_or(
                        "--stabilization-deadline-secs requires a value (the drain deadline seconds)",
                    )?;
                    let seconds: u64 = value.parse().map_err(|_| {
                        format!(
                            "--stabilization-deadline-secs needs a whole number of seconds \
                             (got \"{value}\")"
                        )
                    })?;
                    if seconds == 0 || seconds > MAX_SOAK_WALL_SECONDS {
                        return Err(format!(
                            "--stabilization-deadline-secs must be between 1 and \
                             {MAX_SOAK_WALL_SECONDS} (got {seconds})"
                        ));
                    }
                    options.stabilization_deadline_secs = Some(seconds);
                    index += 2;
                }
                "--scenario" => {
                    let value = args
                        .get(index + 1)
                        .ok_or("--scenario requires a value (the versioned scenario identifier)")?;
                    options.scenario = Some(SoakScenarioId::parse(value)?);
                    index += 2;
                }
                "--scenario-seed" => {
                    let value = args
                        .get(index + 1)
                        .ok_or("--scenario-seed requires a value (the scenario seed)")?;
                    options.scenario_seed = Some(value.parse().map_err(|_| {
                        format!("--scenario-seed needs a whole number (got \"{value}\")")
                    })?);
                    index += 2;
                }
                "--evidence-dir" => {
                    let value = args
                        .get(index + 1)
                        .ok_or("--evidence-dir requires a value (the fresh evidence directory)")?;
                    options.evidence_dir = Some(PathBuf::from(value));
                    index += 2;
                }
                _ => index += 1,
            }
        }
        Ok(options)
    }

    /// The relational rules (the mode context comes from the caller):
    /// every incompatible combination is a visible error.
    pub fn validate(
        &self,
        headless: bool,
        frames: Option<u64>,
        action_at: Option<u64>,
        ticks_given: bool,
    ) -> Result<(), String> {
        let soak = self.wall_seconds.is_some();
        let scenario = self.scenario.is_some();
        if scenario && action_at.is_some() {
            return Err(
                "--scenario and --action-at are incompatible: the scenario owns the \
                 deliberate action's schedule"
                    .into(),
            );
        }
        if soak && headless {
            return Err(
                "--soak-wall-seconds is the GRAPHICAL integrated soak and is incompatible \
                 with --headless (the accelerated headless replay is --scenario alone)"
                    .into(),
            );
        }
        if soak && frames.is_some() {
            return Err(
                "--soak-wall-seconds and --frames are incompatible: the soak's exit is the \
                 measured wall boundary (plus the bounded drain), not a frame count"
                    .into(),
            );
        }
        if soak && ticks_given {
            return Err(
                "--soak-wall-seconds and --ticks are incompatible: --ticks bounds the \
                 headless replay, never the graphical soak"
                    .into(),
            );
        }
        if scenario && !headless && !soak {
            return Err(
                "--scenario requires either --headless (the accelerated logical replay) \
                 or --soak-wall-seconds (the integrated graphical soak)"
                    .into(),
            );
        }
        if self.evidence_dir.is_some() && !scenario {
            return Err("--evidence-dir requires --scenario".into());
        }
        if soak && !scenario {
            return Err("--soak-wall-seconds requires --scenario".into());
        }
        if soak && self.evidence_dir.is_none() {
            return Err("--soak-wall-seconds requires --evidence-dir".into());
        }
        if self.stabilization_deadline_secs.is_some() && !soak {
            return Err(
                "--stabilization-deadline-secs requires --soak-wall-seconds (it bounds \
                 the post-soak fixed-step debt drain)"
                    .into(),
            );
        }
        Ok(())
    }

    /// The effective stabilization deadline.
    pub fn stabilization_deadline(&self) -> u64 {
        self.stabilization_deadline_secs
            .unwrap_or(DEFAULT_STABILIZATION_DEADLINE_SECS)
    }

    /// The effective scenario seed.
    pub fn effective_seed(&self) -> u64 {
        self.scenario_seed.unwrap_or(STAGE6_SCENARIO_SEED)
    }
}
