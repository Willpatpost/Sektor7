//! Stage 4 RegionDirector — proximity-driven LOD over the eight regions
//! with identity-preserving transitions, capture-before-discard, and
//! labeled reconstruction (authorization §4–§6, reviewer priorities
//! A–C). The director owns region levels, checkpoints (references to
//! held `EventId`s — the spine owns the obligations), the in-memory
//! capture ledger, and per-region diagnostics; it never touches the
//! campaign clock and is not a second scheduler — it reshapes the
//! spine's queues at tick boundaries through the narrow Stage 4
//! operations (`suspend_matching`, `resume_matching`,
//! `reclassify_matching`, `schedule_held`).
//!
//! Stage boundary: the capture ledger and checkpoints are the smallest
//! honest **in-memory** mechanisms proving the transition contracts
//! within the active session — NOT canonical commits, NOT durable
//! across process crashes, NOT disk-persisted. The production Commit
//! Pipeline and file-backed persistence arrive in Stage 5.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use engine_core::{
    EventClass, EventId, EventSpec, Fingerprinter, LodConfig, LodConfigError, LodController,
    LodLevel, PresenceSample, ScheduleError, Spine, Tick,
};

use crate::npc::{NPC_COUNT, NpcWorld, REGION_COUNT, payload_activity, payload_npc};

/// In-memory capture-before-discard ledger (E-07/I1 in miniature):
/// keyed captures with pending/applied status and an applied-key ledger
/// — exactly-once application, duplicate attempts are no-ops. Not
/// canonical, not durable (Stage 5 boundary).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaptureLedger {
    /// key -> captured delta (pending until applied).
    pending: BTreeMap<u64, u64>,
    /// Applied-key ledger: keys already applied exactly once.
    applied: BTreeSet<u64>,
}

/// Status of one capture key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStatus {
    Pending(u64),
    Applied,
    Unknown,
}

impl CaptureLedger {
    /// Captures a keyed delta as pending. Idempotent: a duplicate key is
    /// a no-op returning `false`.
    pub fn capture(&mut self, key: u64, delta: u64) -> bool {
        if self.pending.contains_key(&key) || self.applied.contains(&key) {
            return false;
        }
        self.pending.insert(key, delta);
        true
    }

    /// Applies a pending capture exactly once, returning its delta.
    /// `None` for unknown keys or duplicates (no-ops by contract).
    pub fn apply(&mut self, key: u64) -> Option<u64> {
        let delta = self.pending.remove(&key)?;
        self.applied.insert(key);
        Some(delta)
    }

    pub fn status(&self, key: u64) -> CaptureStatus {
        if let Some(delta) = self.pending.get(&key) {
            CaptureStatus::Pending(*delta)
        } else if self.applied.contains(&key) {
            CaptureStatus::Applied
        } else {
            CaptureStatus::Unknown
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn applied_count(&self) -> usize {
        self.applied.len()
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-slice/capture-ledger/1");
        fp.write_usize(self.pending.len());
        for (key, delta) in &self.pending {
            fp.write_u64(*key);
            fp.write_u64(*delta);
        }
        fp.write_usize(self.applied.len());
        for key in &self.applied {
            fp.write_u64(*key);
        }
    }
}

/// Region configuration — demonstration/game policy by contract
/// (authorization §4: thresholds, layout, and the adaptive crossover
/// are configuration, never engine policy).
#[derive(Debug, Clone, PartialEq)]
pub struct RegionConfig {
    /// Region centers in world units. The default layout is a ring of
    /// exact rational coordinates (no trigonometry — fully deterministic
    /// arithmetic).
    pub centers: [(f64, f64); REGION_COUNT],
    /// LOD targeting thresholds, hysteresis bands, and lookahead.
    pub lod: LodConfig,
    /// Adaptive reconstruction threshold: estimated replay activities
    /// per NPC above which synthesis is chosen. Informed by the E-09
    /// crossover (~7.6–12.4 pending events per NPC) — an explicit
    /// experimental configuration, NOT a production policy.
    pub adaptive_threshold: u64,
}

impl Default for RegionConfig {
    fn default() -> Self {
        RegionConfig {
            centers: [
                (100.0, 0.0),
                (75.0, 75.0),
                (0.0, 100.0),
                (-75.0, 75.0),
                (-100.0, 0.0),
                (-75.0, -75.0),
                (0.0, -100.0),
                (75.0, -75.0),
            ],
            lod: LodConfig {
                full_enter: 25.0,
                full_exit: 30.0,
                simplified_enter: 60.0,
                simplified_exit: 70.0,
                event_enter: 120.0,
                event_exit: 140.0,
                lookahead: 1.0,
            },
            adaptive_threshold: 10,
        }
    }
}

/// One NPC's checkpoint entry — a REFERENCE to the spine-held
/// obligation carrying its chain (the spine owns the obligation; this
/// duplicates no authoritative state), plus the routine cursor at
/// suspension for reconstruction math.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionEntry {
    pub npc: usize,
    pub event: EventId,
    pub due: Tick,
    pub cursor: u64,
}

/// A region's dormancy checkpoint: written at deactivation
/// (capture-before-discard), consumed at reactivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionCheckpoint {
    /// Episode idempotency key (region << 32 | episode).
    pub key: u64,
    /// The deactivation tick (the dormant span's label start).
    pub dormant_since: Tick,
    /// The level before dormancy — its cadence governed the suspended
    /// chain's delays and the reconstruction span estimate.
    pub from_level: LodLevel,
    /// Per-NPC held-obligation references (NPC chains only; foreign
    /// payloads stay spine-held without checkpoint entries).
    pub entries: Vec<RegionEntry>,
}

/// The reconstruction method used at an activation — labeled,
/// authoritative state: synthesized spans are never claimed exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconstructionMethod {
    Replay,
    Synthesis,
}

/// Per-region authoritative state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionState {
    pub level: LodLevel,
    /// Deactivation episode counter (idempotency-key derivation).
    pub episode: u64,
    /// Present while dormant; consumed at reactivation.
    pub checkpoint: Option<RegionCheckpoint>,
    /// The last activation's reconstruction method (labeled).
    pub last_method: Option<ReconstructionMethod>,
    /// Reconstruction activities applied at the last activation.
    pub reconstructed_activities: u64,
    /// The last dormancy span in ticks (deactivation → reactivation) —
    /// labeled transition state.
    pub last_dormancy_span: u64,
}

impl RegionState {
    fn empty() -> Self {
        RegionState {
            level: LodLevel::Full,
            episode: 0,
            checkpoint: None,
            last_method: None,
            reconstructed_activities: 0,
            last_dormancy_span: 0,
        }
    }
}

/// Observational region diagnostics (never fingerprinted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegionDiagnostics {
    pub activations: u64,
    pub deactivations: u64,
    pub replay_activations: u64,
    pub synthesis_activations: u64,
    pub reconstructed_activities: u64,
    pub captured_deltas: u64,
    pub reapplied_deltas: u64,
    /// Reported transition anomalies (duplicate activations, promotions
    /// with nothing held) — counted, never silent.
    pub invalid_transitions: u64,
}

/// Deterministic scripted presence for headless verification — explicit
/// simulation inputs, exact-rational arithmetic (no trigonometry), so
/// every platform computes identical positions, projections, and
/// therefore identical LOD decisions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PresenceScenario {
    /// Stationary at a position (velocity zero).
    Stationary { position: (f64, f64) },
    /// Walks from `from` to `to` over `ticks` ticks (velocity is the
    /// per-tick displacement; the lookahead projects along the path).
    Walk {
        from: (f64, f64),
        to: (f64, f64),
        ticks: u64,
    },
    /// Visits every region center in sequence, `ticks_per_region` ticks
    /// each, cycling (velocity is the full hop toward the next center —
    /// the lookahead activates the destination early).
    Tour { ticks_per_region: u64 },
}

impl PresenceScenario {
    /// The presence sample at a tick index.
    pub fn sample_at(&self, tick: u64, centers: &[(f64, f64); REGION_COUNT]) -> PresenceSample {
        match *self {
            PresenceScenario::Stationary { position } => PresenceSample {
                position,
                velocity: (0.0, 0.0),
            },
            PresenceScenario::Walk { from, to, ticks } => {
                let ticks = ticks.max(1);
                let progress = (tick.min(ticks) as f64) / ticks as f64;
                let position = (
                    from.0 + (to.0 - from.0) * progress,
                    from.1 + (to.1 - from.1) * progress,
                );
                let velocity = (
                    (to.0 - from.0) / ticks as f64,
                    (to.1 - from.1) / ticks as f64,
                );
                PresenceSample { position, velocity }
            }
            PresenceScenario::Tour { ticks_per_region } => {
                let per = ticks_per_region.max(1);
                let index = ((tick / per) % REGION_COUNT as u64) as usize;
                let next = centers[(index + 1) % REGION_COUNT];
                let current = centers[index];
                PresenceSample {
                    position: current,
                    velocity: (next.0 - current.0, next.1 - current.1),
                }
            }
        }
    }
}

/// The Stage 4 region director.
pub struct RegionDirector {
    config: RegionConfig,
    controller: LodController,
    states: Vec<RegionState>,
    ledger: CaptureLedger,
    diagnostics: RegionDiagnostics,
    activation_latencies_ms: Vec<f64>,
}

impl RegionDirector {
    pub fn new(config: RegionConfig) -> Result<Self, LodConfigError> {
        let controller = LodController::new(config.lod)?;
        Ok(RegionDirector {
            config,
            controller,
            states: (0..REGION_COUNT).map(|_| RegionState::empty()).collect(),
            ledger: CaptureLedger::default(),
            diagnostics: RegionDiagnostics::default(),
            activation_latencies_ms: Vec::new(),
        })
    }

    pub fn config(&self) -> &RegionConfig {
        &self.config
    }

    /// Current LOD levels, indexed by region.
    pub fn levels(&self) -> [LodLevel; REGION_COUNT] {
        let mut levels = [LodLevel::Full; REGION_COUNT];
        for (region, state) in self.states.iter().enumerate() {
            levels[region] = state.level;
        }
        levels
    }

    pub fn states(&self) -> &[RegionState] {
        &self.states
    }

    pub fn ledger(&self) -> &CaptureLedger {
        &self.ledger
    }

    pub fn diagnostics(&self) -> RegionDiagnostics {
        self.diagnostics
    }

    /// Observational activation-latency percentiles (milliseconds).
    pub fn activation_latency_percentile(&self, quantile: f64) -> f64 {
        if self.activation_latencies_ms.is_empty() {
            return 0.0;
        }
        crate::math::percentile(&self.activation_latencies_ms, quantile)
    }

    /// Schedules a new obligation under the director's policy: dormant
    /// regions take the held path directly (reviewer priority B — new
    /// work targeting a dormant region can never execute unnoticed).
    pub fn schedule(
        &self,
        spine: &mut Spine<NpcWorld>,
        spec: EventSpec,
    ) -> Result<EventId, ScheduleError> {
        let region = spec.class.0 as usize;
        if region < REGION_COUNT && self.states[region].level == LodLevel::Dormant {
            spine.schedule_held(spec)
        } else {
            spine.schedule(spec)
        }
    }

    /// Applies one tick boundary: presence → target LODs (hysteresis in
    /// the controller) → transitions. Deterministic given the explicit
    /// inputs; called before each spine tick. Dormant regions cost
    /// nothing here — one distance computation each, no per-NPC work
    /// (reviewer priority B).
    pub fn apply(&mut self, spine: &mut Spine<NpcWorld>, presence: &[PresenceSample]) {
        let now = spine.clock();
        for region in 0..REGION_COUNT {
            let current = self.states[region].level;
            let next = self
                .controller
                .next_lod(current, self.config.centers[region], presence);
            if next == current {
                continue;
            }
            self.transition(spine, region, current, next, now);
        }
    }

    fn transition(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        region: usize,
        from: LodLevel,
        to: LodLevel,
        now: Tick,
    ) {
        match (from, to) {
            (_, LodLevel::Dormant) => self.deactivate(spine, region, from, now),
            (LodLevel::Dormant, _) => self.activate(spine, region, to, now),
            // Active ↔ active: reclassify to the new lane and update the
            // handler's cadence view. Suspension exists only for Dormant
            // (the approved ladder).
            _ => {
                let class = EventClass::new(region as u16);
                let field = spine.world().config().lod_field(to);
                spine.reclassify_matching(|view| view.class == class, field);
                spine.world_mut().set_region_lod(region, to);
                self.states[region].level = to;
            }
        }
    }

    /// Capture-before-discard deactivation: hold the region's
    /// obligations (identity-preserving), write the checkpoint from the
    /// held view (references only), capture the episode delta, freeze
    /// the handler's cadence view. The dormant region costs the spine
    /// nothing per tick — its obligations are held, its handler never
    /// runs.
    fn deactivate(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        region: usize,
        from: LodLevel,
        now: Tick,
    ) {
        let class = EventClass::new(region as u16);
        spine.suspend_matching(|view| view.class == class);
        spine.world_mut().set_region_lod(region, LodLevel::Dormant);
        let held = spine.held_view();
        let entries: Vec<RegionEntry> = held
            .iter()
            .filter(|view| view.class == class)
            .filter_map(|view| {
                let npc = payload_npc(view.payload);
                if npc < NPC_COUNT {
                    Some(RegionEntry {
                        npc,
                        event: view.id,
                        due: view.due,
                        cursor: payload_activity(view.payload),
                    })
                } else {
                    None
                }
            })
            .collect();
        let episode = self.states[region].episode + 1;
        let key = ((region as u64) << 32) | episode;
        if self.ledger.capture(key, now.as_u64()) {
            self.diagnostics.captured_deltas += 1;
        }
        self.states[region] = RegionState {
            level: LodLevel::Dormant,
            episode,
            checkpoint: Some(RegionCheckpoint {
                key,
                dormant_since: now,
                from_level: from,
                entries,
            }),
            last_method: None,
            reconstructed_activities: 0,
            last_dormancy_span: 0,
        };
        self.diagnostics.deactivations += 1;
    }

    /// Reactivation: reconstruct against the current campaign tick
    /// (labeled replay or synthesis, chosen adaptively), then resume the
    /// held obligations with their original identity and reclassify to
    /// the new lane — the dynamic near/far policy applies immediately
    /// (reviewer priority C: zero priority-transition latency).
    fn activate(&mut self, spine: &mut Spine<NpcWorld>, region: usize, to: LodLevel, now: Tick) {
        let start = Instant::now();
        let class = EventClass::new(region as u16);
        let Some(checkpoint) = self.states[region].checkpoint.take() else {
            // Dormant with nothing held (never deactivated in this
            // session) — a level change only, reported.
            self.diagnostics.invalid_transitions += 1;
            spine.world_mut().set_region_lod(region, to);
            self.states[region].level = to;
            return;
        };
        // Exactly-once episode application: a duplicate activation finds
        // the key applied and is a no-op for the capture ledger.
        let Some(dormant_since) = self.ledger.apply(checkpoint.key) else {
            self.diagnostics.invalid_transitions += 1;
            spine.world_mut().set_region_lod(region, to);
            self.states[region].level = to;
            return;
        };
        let dormancy_span = now.as_u64().saturating_sub(dormant_since);
        // Adaptive selection by estimated replay work per NPC (E-09
        // crossover-informed experimental configuration): the span
        // estimate over the pre-dormancy cadence.
        let cadence = spine.world().config().cadence_factor(checkpoint.from_level);
        let period = spine.world().config().activity_period * cadence.max(1);
        // The work estimate spans from the earliest suspended due —
        // obligations already overdue at deactivation carry more
        // reconstruction work than the dormancy window alone.
        let base = checkpoint
            .entries
            .iter()
            .map(|entry| entry.due.as_u64())
            .min()
            .unwrap_or_else(|| now.as_u64());
        let elapsed = now.as_u64().saturating_sub(base);
        let estimated_per_npc = elapsed / period.max(1);
        let method = if estimated_per_npc > self.config.adaptive_threshold {
            ReconstructionMethod::Synthesis
        } else {
            ReconstructionMethod::Replay
        };
        // Reconstruction: labeled routine-state advancement only —
        // never steps, milestones, or the execution ledger.
        let mut reconstructed = 0u64;
        {
            let world = spine.world_mut();
            for entry in &checkpoint.entries {
                let count = match method {
                    ReconstructionMethod::Replay => {
                        world.replay_routine_span(entry.npc, entry.due, now, cadence)
                    }
                    ReconstructionMethod::Synthesis => {
                        world.synthesize_routine_span(entry.npc, entry.due, now, cadence)
                    }
                };
                reconstructed += count;
            }
        }
        // Resume with original identity, then reclassify to the
        // activation's lane — the reservation protects the region
        // immediately.
        spine.resume_matching(|view| view.class == class);
        let field = spine.world().config().lod_field(to);
        spine.reclassify_matching(|view| view.class == class, field);
        spine.world_mut().set_region_lod(region, to);
        self.states[region] = RegionState {
            level: to,
            episode: self.states[region].episode,
            checkpoint: None,
            last_method: Some(method),
            reconstructed_activities: reconstructed,
            last_dormancy_span: dormancy_span,
        };
        self.diagnostics.activations += 1;
        self.diagnostics.reconstructed_activities += reconstructed;
        self.diagnostics.reapplied_deltas += 1;
        match method {
            ReconstructionMethod::Replay => self.diagnostics.replay_activations += 1,
            ReconstructionMethod::Synthesis => self.diagnostics.synthesis_activations += 1,
        }
        self.activation_latencies_ms
            .push(start.elapsed().as_secs_f64() * 1000.0);
        let len = self.activation_latencies_ms.len();
        if len > 128 {
            self.activation_latencies_ms.drain(0..len - 128);
        }
    }

    /// Stage 4 authoritative region-state fingerprint (domain
    /// "engine-slice/region-state/1"): per-region level, episode,
    /// checkpoint references, reconstruction labels and counts, plus
    /// the capture ledger. Observational diagnostics and latency
    /// history are excluded.
    pub fn region_fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-slice/region-state/1");
        for state in &self.states {
            fp.write_u8(lod_byte(state.level));
            fp.write_u64(state.episode);
            fp.write_u8(u8::from(state.checkpoint.is_some()));
            if let Some(checkpoint) = &state.checkpoint {
                fp.write_u64(checkpoint.key);
                fp.write_tick(checkpoint.dormant_since);
                fp.write_u8(lod_byte(checkpoint.from_level));
                fp.write_usize(checkpoint.entries.len());
                for entry in &checkpoint.entries {
                    fp.write_usize(entry.npc);
                    fp.write_u64(entry.event.seq);
                    fp.write_tick(entry.due);
                    fp.write_u64(entry.cursor);
                }
            }
            fp.write_u8(match state.last_method {
                None => 0,
                Some(ReconstructionMethod::Replay) => 1,
                Some(ReconstructionMethod::Synthesis) => 2,
            });
            fp.write_u64(state.reconstructed_activities);
            fp.write_u64(state.last_dormancy_span);
        }
        self.ledger.fingerprint(fp);
    }
}

/// The authoritative Stage 4 combined state fingerprint (domain
/// "engine-slice/stage4-state/1"): the spine's lane-state fingerprint
/// (which includes the frozen `npc-world/1` domain), the held-state
/// fingerprint, the world's `npc-spatial/1` domain, and the
/// `region-state/1` domain. Historical Stage 0/3 domains remain
/// compatibility baselines; THIS is the authoritative comparison for
/// Stage 4 states (reviewer clarification C).
pub fn stage4_state_fingerprint(spine: &Spine<NpcWorld>, director: &RegionDirector) -> u64 {
    let mut fp = Fingerprinter::new();
    fp.write_str("engine-slice/stage4-state/1");
    fp.write_u64(spine.lane_state_fingerprint());
    fp.write_u64(spine.held_fingerprint());
    spine.world().spatial_fingerprint(&mut fp);
    director.region_fingerprint(&mut fp);
    fp.finish()
}

pub fn stage4_state_fingerprint_hex(spine: &Spine<NpcWorld>, director: &RegionDirector) -> String {
    format!("{:016x}", stage4_state_fingerprint(spine, director))
}

fn lod_byte(level: LodLevel) -> u8 {
    match level {
        LodLevel::Full => 0,
        LodLevel::Simplified => 1,
        LodLevel::EventDriven => 2,
        LodLevel::Dormant => 3,
    }
}
