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

use crate::npc::{NpcWorld, REGION_COUNT, routine_payload_parts};

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

/// Outcome of an exactly-once application attempt (Batch 2 corrective
/// finding C: application success is explicit, so activation can fail
/// coherently instead of half-promoting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Applied now — the delta is returned for commitment.
    Applied(u64),
    /// Already applied earlier: a duplicate attempt, a no-op.
    AlreadyApplied,
}

/// An application failure: the key is neither pending nor applied —
/// the checkpoint claims a consequence the ledger never captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyError {
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

    /// Applies a pending capture exactly once. A duplicate attempt is
    /// `Ok(AlreadyApplied)` (a no-op by contract); an unknown key is an
    /// `Err` — the caller's recovery state references a consequence the
    /// ledger never captured, which must fail coherently rather than
    /// silently promote.
    pub fn apply(&mut self, key: u64) -> Result<ApplyOutcome, ApplyError> {
        let Some(delta) = self.pending.remove(&key) else {
            if self.applied.contains(&key) {
                return Ok(ApplyOutcome::AlreadyApplied);
            }
            return Err(ApplyError::Unknown);
        };
        self.applied.insert(key);
        Ok(ApplyOutcome::Applied(delta))
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

    /// Sum of unapplied captured deltas — the consequence-conservation
    /// term for dormant regions (corrective finding A's proof).
    pub fn pending_total(&self) -> u64 {
        self.pending.values().sum()
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        // Domain version 2 (Batch 2 corrective): keys moved from
        // per-episode timestamps to per-NPC consequence deltas.
        fp.write_str("engine-slice/capture-ledger/2");
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

/// Capture key for one NPC's consequences in one deactivation episode:
/// region (bits 54+) | episode (bits 10+) | npc. Episodes increment per
/// deactivation, so every episode's captures are keyed fresh; the
/// applied-key ledger is the exactly-once guard across retries and
/// duplicate activation attempts.
pub fn consequence_key(region: usize, episode: u64, npc: usize) -> u64 {
    ((region as u64) << 54) | (episode << 10) | (npc as u64)
}

/// One NPC's checkpoint entry — a REFERENCE to the spine-held
/// obligation carrying its chain (the spine owns the obligation with
/// its original EventId, due, schedule, dependencies, and lane; this
/// duplicates no authoritative state), plus the reconstructible state
/// captured at suspension for reconstruction (corrective findings A
/// and D — the world's copies are discarded at deactivation;
/// reactivation restores from here): the CURRENT represented routine
/// cursor and grid position, and the represented-through boundary —
/// the coherent anchor a later reconstruction counts FROM, so an
/// already-represented interval is never reconstructed again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionEntry {
    pub npc: usize,
    pub event: EventId,
    /// The represented-through boundary (corrective finding D): the
    /// logical time through which this NPC's routine progression is
    /// represented — the obligation's own due while the chain is
    /// coherent (its payload cursor matches the world cursor, so the
    /// next activity is genuinely pending at that due), otherwise the
    /// reconstruction boundary tracked by the world. Reconstruction
    /// counts only activities strictly after this anchor; the pending
    /// obligation's own activity is performed by real execution.
    pub represented_through: Tick,
    /// The represented routine cursor captured from the WORLD at
    /// deactivation — never reinitialized from a stale pending payload
    /// (corrective finding D).
    pub cursor: u64,
    /// The grid position captured at deactivation (reconstructible
    /// state held by the checkpoint).
    pub position: (u8, u8),
    /// The checkpoint's claim that this NPC carried an uncommitted
    /// consequential signal at deactivation — the ledger owns the
    /// exactly-once application; a claimed key unknown to the ledger
    /// fails activation coherently (corrective finding C).
    pub has_signal: bool,
}

/// A region's dormancy checkpoint: written at deactivation
/// (capture-before-discard), consumed at reactivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionCheckpoint {
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
    /// Test-only fault injection (corrective finding C): makes the
    /// NAMED region's next activation fail its capture application
    /// BEFORE any mutation — other regions activating at the same tick
    /// boundary are unaffected. Engine code never sets it.
    fail_next_apply_region: Option<usize>,
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
            fail_next_apply_region: None,
        })
    }

    /// Test-only fault injection (corrective finding C): the named
    /// region's next activation fails its capture application before
    /// any state changes, leaving that region safely dormant with its
    /// checkpoint and held obligations intact — the coherent-failure
    /// contract under test. Engine code never calls this.
    #[doc(hidden)]
    pub fn debug_inject_apply_failure(&mut self, region: usize) {
        self.fail_next_apply_region = Some(region);
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

    /// Capture-before-discard deactivation (corrective findings A and
    /// D): hold the region's obligations (identity-preserving), write
    /// the checkpoint from the held view (references plus the captured
    /// represented cursor/position and represented-through boundary —
    /// routine-typed chains only), capture every uncommitted
    /// consequential signal into the in-memory ledger, and only then
    /// DISCARD the region's reconstructible routine representation and
    /// signal scratch. The dormant region costs the spine nothing per
    /// tick — its obligations are held, its handler never runs.
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
        // Gather the region's held references first (the spine owns the
        // obligations); only routine-typed payloads become checkpoint
        // entries — foreign events stay spine-held without entries.
        let held: Vec<(EventId, Tick, u64)> = spine
            .held_view()
            .iter()
            .filter(|view| view.class == class)
            .map(|view| (view.id, view.due, view.payload))
            .collect();
        let episode = self.states[region].episode + 1;
        let mut entries = Vec::with_capacity(held.len());
        let mut captures: Vec<(usize, u64)> = Vec::new();
        {
            let world = spine.world();
            for (event, due, payload) in &held {
                if let Some((npc, payload_cursor)) = routine_payload_parts(*payload) {
                    let pending = world.pending_signals()[npc];
                    // Corrective finding D: capture the CURRENT
                    // represented state from the world — never a stale
                    // pending payload. The coherent anchor is the
                    // obligation's own due while the chain is coherent
                    // (payload cursor == world cursor: the state was
                    // last advanced by real execution, which re-bases
                    // the chain); once reconstruction advanced the
                    // state past the obligation's knowledge, the
                    // world's tracked represented-through boundary is
                    // the anchor, and a later reconstruction counts
                    // only the unrepresented interval.
                    let world_cursor = world.routine_cursor()[npc];
                    let represented_through = if payload_cursor == world_cursor {
                        *due
                    } else {
                        world.represented_through()[npc]
                    };
                    entries.push(RegionEntry {
                        npc,
                        event: *event,
                        represented_through,
                        cursor: world_cursor,
                        position: world.positions()[npc],
                        has_signal: pending > 0,
                    });
                    if pending > 0 {
                        captures.push((npc, pending));
                    }
                }
            }
        }
        // Capture BEFORE discard: every uncommitted consequential signal
        // is captured into the in-memory ledger (keyed per region,
        // episode, and NPC — exactly-once application, explicit
        // pending status) — only then is the scratch and the
        // reconstructible representation discarded.
        for (npc, delta) in captures {
            if self
                .ledger
                .capture(consequence_key(region, episode, npc), delta)
            {
                self.diagnostics.captured_deltas += 1;
            }
        }
        spine.world_mut().discard_region_routine(region);
        self.states[region] = RegionState {
            level: LodLevel::Dormant,
            episode,
            checkpoint: Some(RegionCheckpoint {
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

    /// Reactivation (corrective findings A and C): commit the captured
    /// consequences exactly once, reconstruct against the current
    /// campaign tick (labeled replay or synthesis, chosen adaptively,
    /// seeded from the checkpoint's captured cursor/position), then
    /// resume the held obligations with their original identity and
    /// reclassify to the new lane — the dynamic near/far policy applies
    /// immediately (reviewer priority C: zero priority-transition
    /// latency). Validation precedes mutation: an activation that
    /// cannot apply its captures coherently leaves the region safely
    /// dormant — checkpoint retained, obligations still held, level
    /// unchanged — never half-promoted.
    fn activate(&mut self, spine: &mut Spine<NpcWorld>, region: usize, to: LodLevel, now: Tick) {
        let class = EventClass::new(region as u16);
        // Phase 0 — validation, no state mutations (the fault-injection
        // flag is test-only state).
        let Some(checkpoint) = self.states[region].checkpoint.clone() else {
            // Dormant with nothing held (never deactivated in this
            // session) — a level change only, reported.
            self.diagnostics.invalid_transitions += 1;
            spine.world_mut().set_region_lod(region, to);
            self.states[region].level = to;
            return;
        };
        if self.fail_next_apply_region == Some(region) {
            self.fail_next_apply_region = None;
            self.diagnostics.invalid_transitions += 1;
            return;
        }
        let episode = self.states[region].episode;
        for entry in checkpoint.entries.iter().filter(|entry| entry.has_signal) {
            if matches!(
                self.ledger
                    .status(consequence_key(region, episode, entry.npc)),
                CaptureStatus::Unknown
            ) {
                // The checkpoint claims a consequence the ledger never
                // captured — a coherent failure: the region stays
                // dormant with its checkpoint and held obligations
                // intact.
                self.diagnostics.invalid_transitions += 1;
                return;
            }
        }
        let start = Instant::now();
        // Phase 1 — commit captured consequences exactly once. The
        // applied-key ledger makes applications idempotent, so a
        // retried activation never double-commits.
        for entry in checkpoint.entries.iter().filter(|entry| entry.has_signal) {
            match self
                .ledger
                .apply(consequence_key(region, episode, entry.npc))
            {
                Ok(ApplyOutcome::Applied(delta)) => {
                    if delta > 0 {
                        spine.world_mut().apply_captured_signal(entry.npc, delta);
                        self.diagnostics.reapplied_deltas += 1;
                    }
                }
                Ok(ApplyOutcome::AlreadyApplied) => {}
                Err(ApplyError::Unknown) => {
                    // Unreachable after validation; treated as a
                    // coherent failure. Keys already applied stay
                    // applied, so a retry commits only the remainder.
                    self.diagnostics.invalid_transitions += 1;
                    return;
                }
            }
        }
        let dormancy_span = now
            .as_u64()
            .saturating_sub(checkpoint.dormant_since.as_u64());
        // Adaptive selection by estimated replay work per NPC (E-09
        // crossover-informed experimental configuration): the span
        // estimate over the pre-dormancy cadence.
        let cadence = spine.world().config().cadence_factor(checkpoint.from_level);
        let period = spine.world().config().activity_period * cadence.max(1);
        // The work estimate spans from the earliest represented-through
        // boundary — the UNREPRESENTED interval is what reconstruction
        // must cover (corrective finding D: anchors already advanced
        // past the historical dues by prior reconstruction estimate
        // only the remaining work, never the full dormant span again).
        let base = checkpoint
            .entries
            .iter()
            .map(|entry| entry.represented_through.as_u64())
            .min()
            .unwrap_or_else(|| now.as_u64());
        let elapsed = now.as_u64().saturating_sub(base);
        let estimated_per_npc = elapsed / period.max(1);
        let method = if estimated_per_npc > self.config.adaptive_threshold {
            ReconstructionMethod::Synthesis
        } else {
            ReconstructionMethod::Replay
        };
        // Phase 2 — reconstruction (corrective finding D): restore each
        // chain's captured cursor/position AND represented-through
        // boundary from the checkpoint (the world's copies were
        // DISCARDED at deactivation), then reconstruct ONLY the
        // previously unrepresented interval — counting strictly after
        // the boundary, so a span already represented at an earlier
        // activation is never reconstructed again. Labeled
        // routine-state reconstruction only; never steps, milestones,
        // credits, or the execution ledger.
        let mut reconstructed = 0u64;
        {
            let world = spine.world_mut();
            for entry in &checkpoint.entries {
                world.restore_routine(
                    entry.npc,
                    entry.cursor,
                    entry.position,
                    entry.represented_through,
                );
                let count = match method {
                    ReconstructionMethod::Replay => world.replay_routine_span(
                        entry.npc,
                        entry.represented_through,
                        now,
                        cadence,
                    ),
                    ReconstructionMethod::Synthesis => world.synthesize_routine_span(
                        entry.npc,
                        entry.represented_through,
                        now,
                        cadence,
                    ),
                };
                reconstructed += count;
            }
        }
        // Phase 3 — resume with original identity, reclassify to the
        // activation's lane (the reservation protects the region
        // immediately), and promote.
        spine.resume_matching(|view| view.class == class);
        let field = spine.world().config().lod_field(to);
        spine.reclassify_matching(|view| view.class == class, field);
        spine.world_mut().set_region_lod(region, to);
        self.states[region] = RegionState {
            level: to,
            episode,
            checkpoint: None,
            last_method: Some(method),
            reconstructed_activities: reconstructed,
            last_dormancy_span: dormancy_span,
        };
        self.diagnostics.activations += 1;
        self.diagnostics.reconstructed_activities += reconstructed;
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
    /// "engine-slice/region-state/3" — version 3 per the Batch 2
    /// corrective finding D: checkpoint entries carry the
    /// represented-through boundary instead of the raw obligation due,
    /// and the cursor is the world-captured represented state): per-
    /// region level, episode, checkpoint references, reconstruction
    /// labels and counts, plus the capture ledger. Observational
    /// diagnostics and latency history are excluded.
    pub fn region_fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-slice/region-state/3");
        for state in &self.states {
            fp.write_u8(lod_byte(state.level));
            fp.write_u64(state.episode);
            fp.write_u8(u8::from(state.checkpoint.is_some()));
            if let Some(checkpoint) = &state.checkpoint {
                fp.write_tick(checkpoint.dormant_since);
                fp.write_u8(lod_byte(checkpoint.from_level));
                fp.write_usize(checkpoint.entries.len());
                for entry in &checkpoint.entries {
                    fp.write_usize(entry.npc);
                    fp.write_u64(entry.event.seq);
                    fp.write_tick(entry.represented_through);
                    fp.write_u64(entry.cursor);
                    fp.write_u8(entry.position.0);
                    fp.write_u8(entry.position.1);
                    fp.write_u8(u8::from(entry.has_signal));
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
