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

use std::collections::BTreeMap;
use std::time::Instant;

use engine_core::wire::MAX_COLLECTION_COUNT;
use engine_core::{
    Decoder, Encoder, EventClass, EventId, EventSpec, Fingerprinter, LodConfig, LodConfigError,
    LodController, LodLevel, ObligationView, PresenceSample, SaveError, ScheduleError, Spine, Tick,
};
use engine_persist::ConsequenceId;

use crate::npc::{
    NPC_COUNT, NpcWorld, POSITION_DISCARDED, REGION_COUNT, REGION_GRID, routine_payload_parts,
};

/// In-memory capture-before-discard ledger (E-07/I1 in miniature):
/// keyed captures with pending/applied status and an applied-key ledger
/// — exactly-once application, duplicate attempts are no-ops. Not
/// canonical, not durable (the Stage 5 boundary; the Batch 4 crossing
/// journals through the engine-persist pipeline separately).
///
/// **The structured-key migration (Batch 4, the accepted §5.5 design):
/// the keys are `ConsequenceId`s in memory** — (region, episode, npc)
/// structured identities — while the fingerprint's wire form is the
/// packed Stage 4 encoding: `ConsequenceId::pack_stage4` IS the legacy
/// `consequence_key` expression (the bijection the engine-persist
/// boundary trio pinned against this crate's own layout, proven live
/// by the crossing's compatibility test), and the structured field
/// order (region, episode, npc) coincides with the packed numeric
/// order (field-aligned bit packing), so the `capture-ledger/2` domain
/// keeps byte-identical output — no /3 storage-encoding change is
/// needed, because the packed form already IS the structured key's
/// canonical packed form. Historical behavior and every frozen Stage 4
/// fingerprint are mechanically preserved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaptureLedger {
    /// id -> captured delta (pending until applied).
    pending: BTreeMap<ConsequenceId, u64>,
    /// id -> applied amount: the exactly-once applied-key ledger,
    /// carrying the per-identity amount as the exact materialization
    /// evidence (Batch 4's second review corrective: the applied
    /// lane's amount must survive verified snapshot publication
    /// independently of the historical T1 journal — never inferred
    /// from aggregate counters).
    applied: BTreeMap<ConsequenceId, u64>,
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
    /// Captures a keyed delta as pending. Idempotent: a duplicate id is
    /// a no-op returning `false`.
    pub fn capture(&mut self, id: ConsequenceId, delta: u64) -> bool {
        if self.pending.contains_key(&id) || self.applied.contains_key(&id) {
            return false;
        }
        self.pending.insert(id, delta);
        true
    }

    /// Applies a pending capture exactly once. A duplicate attempt is
    /// `Ok(AlreadyApplied)` (a no-op by contract); an unknown id is an
    /// `Err` — the caller's recovery state references a consequence the
    /// ledger never captured, which must fail coherently rather than
    /// silently promote.
    pub fn apply(&mut self, id: ConsequenceId) -> Result<ApplyOutcome, ApplyError> {
        let Some(delta) = self.pending.remove(&id) else {
            if self.applied.contains_key(&id) {
                return Ok(ApplyOutcome::AlreadyApplied);
            }
            return Err(ApplyError::Unknown);
        };
        self.applied.insert(id, delta);
        Ok(ApplyOutcome::Applied(delta))
    }

    pub fn status(&self, id: ConsequenceId) -> CaptureStatus {
        if let Some(delta) = self.pending.get(&id) {
            CaptureStatus::Pending(*delta)
        } else if self.applied.contains_key(&id) {
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

    /// The pending lane's per-identity entries (canonical order).
    pub(crate) fn pending_entries(&self) -> impl Iterator<Item = (ConsequenceId, u64)> + '_ {
        self.pending.iter().map(|(id, delta)| (*id, *delta))
    }

    /// The applied lane's per-identity entries with their amounts
    /// (canonical order) — the exact materialization evidence.
    pub(crate) fn applied_entries(&self) -> impl Iterator<Item = (ConsequenceId, u64)> + '_ {
        self.applied.iter().map(|(id, amount)| (*id, *amount))
    }

    /// Sum of unapplied captured deltas — the consequence-conservation
    /// term for dormant regions (corrective finding A's proof).
    pub fn pending_total(&self) -> u64 {
        self.pending.values().sum()
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        // Domain version 2 (Batch 2 corrective): keys moved from
        // per-episode timestamps to per-NPC consequence deltas. Batch 4:
        // the in-memory keys are structured ConsequenceIds; the wire
        // form is their packed Stage 4 encoding — byte-identical to the
        // legacy u64 keys (the bijection plus the field-aligned
        // ordering), so the domain stays /2 with unchanged output.
        fp.write_str("engine-slice/capture-ledger/2");
        fp.write_usize(self.pending.len());
        for (id, delta) in &self.pending {
            fp.write_u64(pack_crossing(*id));
            fp.write_u64(*delta);
        }
        // The applied lane's ids only: the per-identity amounts are
        // deliberately excluded from the frozen /2 fingerprint (the
        // accepted pins stay byte-identical); the amounts' exact
        // evidence lives in the persisted section's in-band v3
        // schema (the save path below).
        fp.write_usize(self.applied.len());
        for id in self.applied.keys() {
            fp.write_u64(pack_crossing(*id));
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
///
/// Batch 4 (the structured-key migration): the crossing now speaks
/// `ConsequenceId` — this legacy expression remains as the layout
/// documentation and the compatibility oracle (the crossing test proves
/// `ConsequenceId::pack_stage4` equals it for every in-domain triple,
/// which is what keeps `capture-ledger/2` byte-identical).
pub fn consequence_key(region: usize, episode: u64, npc: usize) -> u64 {
    ((region as u64) << 54) | (episode << 10) | (npc as u64)
}

/// The crossing's pack invariant: the director's identities are
/// constructed from REGION_COUNT/NPC_COUNT-bounded values, so
/// `pack_stage4` is total on them — the strict bounds (reject, never
/// wrap) guard the general API against out-of-domain identities, and
/// the compatibility test pins the bijection against this crate's own
/// legacy layout.
fn pack_crossing(id: ConsequenceId) -> u64 {
    id.pack_stage4().expect(
        "the crossing's identity domain is packable (region < REGION_COUNT, npc < NPC_COUNT)",
    )
}

/// The structured identity for one NPC's consequences in one
/// deactivation episode — the Batch 4 crossing's key form.
fn crossing_id(region: usize, episode: u64, npc: usize) -> ConsequenceId {
    ConsequenceId::new(region as u32, episode, npc as u32)
}

/// Gathers one region's deactivation checkpoint entries and captured
/// consequences from its obligation views (Batch 4: extracted from the
/// legacy deactivation so the six-phase crossing's read-only
/// preparation builds the SAME plan data — `Spine::collect_matching`
/// feeds the durable path, `held_view` the legacy path; identical
/// gathering, identical order). The views are normalized to ascending
/// `EventId` order — the legacy held iteration's form — so both paths
/// produce identical checkpoints. Only routine-typed payloads become
/// checkpoint entries — foreign events stay spine-held without entries
/// (corrective finding A); the class↔region consistency check (Batch 3
/// defensive check) keeps a malformed routine-tagged payload whose NPC
/// belongs to another region out of THIS region's reconstruction data.
pub(crate) fn gather_region_entries(
    world: &NpcWorld,
    region: usize,
    obligations: &[ObligationView<'_>],
) -> (Vec<RegionEntry>, Vec<(usize, u64)>) {
    let mut ordered: Vec<&ObligationView<'_>> = obligations.iter().collect();
    ordered.sort_by_key(|view| view.id.seq);
    let mut entries = Vec::with_capacity(ordered.len());
    let mut captures = Vec::new();
    for view in ordered {
        let Some((npc, payload_cursor)) = routine_payload_parts(view.payload) else {
            continue;
        };
        if npc % REGION_COUNT != region {
            continue;
        }
        let pending = world.pending_signals()[npc];
        // Corrective finding D: capture the CURRENT represented state
        // from the world — never a stale pending payload. The coherent
        // anchor is the obligation's own due while the chain is
        // coherent (payload cursor == world cursor: the state was last
        // advanced by real execution, which re-bases the chain); once
        // reconstruction advanced the state past the obligation's
        // knowledge, the world's tracked represented-through boundary
        // is the anchor, and a later reconstruction counts only the
        // unrepresented interval.
        let world_cursor = world.routine_cursor()[npc];
        let represented_through = if payload_cursor == world_cursor {
            view.due
        } else {
            world.represented_through()[npc]
        };
        entries.push(RegionEntry {
            npc,
            event: view.id,
            represented_through,
            cursor: world_cursor,
            position: world.positions()[npc],
            has_signal: pending > 0,
        });
        if pending > 0 {
            captures.push((npc, pending));
        }
    }
    (entries, captures)
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

    /// Test-only state corruption (Batch 3 defensive check 2): clears
    /// the named region's recovery checkpoint, so its next activation
    /// hits the fail-closed path — no checkpoint means the suspended
    /// state cannot be reconstructed, and the region must stay
    /// dormant. Unreachable through normal transitions (deactivate
    /// always writes a checkpoint); engine code never calls this.
    #[doc(hidden)]
    pub fn debug_clear_checkpoint(&mut self, region: usize) {
        self.states[region].checkpoint = None;
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

    /// The LOD the controller would transition `region` to under this
    /// presence (read-only — the decision input the six-phase
    /// crossing's boundary routing consults; never a mutation).
    pub fn target_level(&self, region: usize, presence: &[PresenceSample]) -> LodLevel {
        self.controller.next_lod(
            self.states[region].level,
            self.config.centers[region],
            presence,
        )
    }

    /// Transitions one region to `to` — the LEGACY semantics (the same
    /// routing `apply` performs per region; the caller's presence
    /// decision), returning whether the region's level actually
    /// changed (a coherent activation failure leaves the region
    /// dormant — false; a no-op target is false). The six-phase
    /// crossing routes its Dormant-target cases through
    /// `SessionCrossing::deactivate` and its reactivations through
    /// `SessionCrossing::activate`; this method serves the remaining
    /// transitions (active↔active reclassification) and the
    /// pipeline-free path.
    pub fn transition_region(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        region: usize,
        to: LodLevel,
        now: Tick,
    ) -> bool {
        let from = self.states[region].level;
        if to == from {
            return false;
        }
        self.transition(spine, region, from, to, now);
        self.states[region].level != from
    }

    /// The six-phase crossing's phase-5 install step (Batch 4):
    /// captures the plan's consequences into the ledger — exactly-once
    /// keyed, the diagnostics parity with the legacy path — called
    /// only after the staged precondition re-validation.
    pub(crate) fn install_captures(
        &mut self,
        region: usize,
        episode: u64,
        captures: &[(usize, u64)],
    ) {
        for (npc, delta) in captures {
            if self
                .ledger
                .capture(crossing_id(region, episode, *npc), *delta)
            {
                self.diagnostics.captured_deltas += 1;
            }
        }
    }

    /// The six-phase crossing's phase-5 install step (Batch 4): writes
    /// the deactivated region state from the prepared plan — the same
    /// state the legacy path writes (level, episode, checkpoint),
    /// plus the diagnostics parity.
    pub(crate) fn install_deactivated(
        &mut self,
        region: usize,
        episode: u64,
        checkpoint: RegionCheckpoint,
    ) {
        self.states[region] = RegionState {
            level: LodLevel::Dormant,
            episode,
            checkpoint: Some(checkpoint),
            last_method: None,
            reconstructed_activities: 0,
            last_dormancy_span: 0,
        };
        self.diagnostics.deactivations += 1;
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
        // Gather the region's held references (the spine owns the
        // obligations) through the SHARED gathering — the same
        // read-only construction the six-phase crossing's preparation
        // performs from the collected views (Batch 4: identical
        // entries and captures, identical order — the helper's
        // ascending-seq normalization is a no-op on the held view).
        let held: Vec<ObligationView<'_>> = spine
            .held_view()
            .into_iter()
            .filter(|view| view.class == class)
            .collect();
        let episode = self.states[region].episode + 1;
        let (entries, captures) = gather_region_entries(spine.world(), region, &held);
        // Capture BEFORE discard: every uncommitted consequential signal
        // is captured into the in-memory ledger (keyed per region,
        // episode, and NPC — exactly-once application, explicit
        // pending status) — only then is the scratch and the
        // reconstructible representation discarded.
        for (npc, delta) in captures {
            if self
                .ledger
                .capture(crossing_id(region, episode, npc), delta)
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
            // No recovery checkpoint: the region's suspended state
            // cannot be reconstructed — fail CLOSED (Batch 3 defensive
            // check 2). The region stays dormant with its held
            // obligations untouched and the anomaly reported — never a
            // silent promotion of unreconstructible state. (Unreachable
            // through normal transitions: deactivate always writes a
            // checkpoint.)
            self.diagnostics.invalid_transitions += 1;
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
                self.ledger.status(crossing_id(region, episode, entry.npc)),
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
            match self.ledger.apply(crossing_id(region, episode, entry.npc)) {
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

impl RegionDirector {
    /// The director's authoritative-state codec — the session
    /// snapshot's director section (Batch 4, the second
    /// authoritative-state codec): the full behavioral configuration
    /// (re-validated on restore through [`RegionDirector::new`]), the
    /// per-region states (levels, episode counters, dormancy
    /// checkpoints with their held-obligation references and captured
    /// reconstructible state), and the structured-keyed capture ledger
    /// (in-flight and applied identities, canonical
    /// `(region, episode, npc)` order). Observational diagnostics and
    /// latency history are excluded — never fingerprinted, never
    /// saved. Domain `engine-slice/region-director-save/1`.
    pub fn save(&self, enc: &mut Encoder) {
        // The behavioral configuration — transition identity.
        enc.write_count(REGION_COUNT);
        for (x, y) in &self.config.centers {
            enc.write_u64(x.to_bits());
            enc.write_u64(y.to_bits());
        }
        let lod = &self.config.lod;
        for value in [
            lod.full_enter,
            lod.full_exit,
            lod.simplified_enter,
            lod.simplified_exit,
            lod.event_enter,
            lod.event_exit,
            lod.lookahead,
        ] {
            enc.write_u64(value.to_bits());
        }
        enc.write_u64(self.config.adaptive_threshold);
        // The per-region authoritative states.
        enc.write_count(REGION_COUNT);
        for state in &self.states {
            enc.write_u8(lod_byte(state.level));
            enc.write_u64(state.episode);
            enc.write_u8(u8::from(state.checkpoint.is_some()));
            if let Some(checkpoint) = &state.checkpoint {
                enc.write_tick(checkpoint.dormant_since);
                enc.write_u8(lod_byte(checkpoint.from_level));
                enc.write_count(checkpoint.entries.len());
                for entry in &checkpoint.entries {
                    enc.write_u64(entry.npc as u64);
                    enc.write_u64(entry.event.seq);
                    enc.write_tick(entry.represented_through);
                    enc.write_u64(entry.cursor);
                    enc.write_u8(entry.position.0);
                    enc.write_u8(entry.position.1);
                    enc.write_u8(u8::from(entry.has_signal));
                }
            }
            enc.write_u8(match state.last_method {
                None => 0,
                Some(ReconstructionMethod::Replay) => 1,
                Some(ReconstructionMethod::Synthesis) => 2,
            });
            enc.write_u64(state.reconstructed_activities);
            enc.write_u64(state.last_dormancy_span);
        }
        // The capture ledger: structured identities, canonical
        // (region, episode, npc) order (the BTreeMap iteration). The
        // section's schema version is explicit in-band: v3 adds the
        // per-identity APPLIED amounts — the exact materialization
        // evidence that survives verified snapshot publication
        // independently of the historical T1 journal (the second
        // review corrective; v2's ids-only applied form never
        // shipped in an accepted build, so the walk reads v3 only).
        enc.write_u8(3);
        enc.write_count(self.ledger.pending.len());
        for (id, delta) in &self.ledger.pending {
            enc.write_u32(id.region);
            enc.write_u64(id.episode);
            enc.write_u32(id.npc);
            enc.write_u64(*delta);
        }
        enc.write_count(self.ledger.applied.len());
        for (id, amount) in &self.ledger.applied {
            enc.write_u32(id.region);
            enc.write_u64(id.episode);
            enc.write_u32(id.npc);
            enc.write_u64(*amount);
        }
    }

    /// Restores a director from its section bytes (fail-closed: every
    /// count bounded before allocation, every discriminant typed, the
    /// identity and grid bounds enforced, the ledger's canonical
    /// strict-ascending order required — duplicates and reorders are
    /// corruption — and the configuration re-validated through
    /// [`RegionDirector::new`]).
    pub fn restore(dec: &mut Decoder) -> Result<Self, SaveError> {
        const SECTION: &str = "region-director-save";
        let center_count = dec.read_count(REGION_COUNT as u64, SECTION, "center-count")?;
        if center_count != REGION_COUNT as u64 {
            return Err(SaveError::Integrity("region-director-save/center-count"));
        }
        let mut centers = [(0.0, 0.0); REGION_COUNT];
        for center in &mut centers {
            let x = read_finite_f64(dec)?;
            let y = read_finite_f64(dec)?;
            *center = (x, y);
        }
        let lod = LodConfig {
            full_enter: read_finite_f64(dec)?,
            full_exit: read_finite_f64(dec)?,
            simplified_enter: read_finite_f64(dec)?,
            simplified_exit: read_finite_f64(dec)?,
            event_enter: read_finite_f64(dec)?,
            event_exit: read_finite_f64(dec)?,
            lookahead: read_finite_f64(dec)?,
        };
        let adaptive_threshold = dec.read_u64()?;
        let config = RegionConfig {
            centers,
            lod,
            adaptive_threshold,
        };
        let state_count = dec.read_count(REGION_COUNT as u64, SECTION, "state-count")?;
        if state_count != REGION_COUNT as u64 {
            return Err(SaveError::Integrity("region-director-save/state-count"));
        }
        let mut states = Vec::with_capacity(REGION_COUNT);
        for _ in 0..REGION_COUNT {
            let level = read_lod_level(dec, SECTION)?;
            let episode = dec.read_u64()?;
            let checkpoint_present = read_flag(dec, SECTION)?;
            let checkpoint = if checkpoint_present {
                let dormant_since = dec.read_tick()?;
                let from_level = read_lod_level(dec, SECTION)?;
                let entry_count = dec.read_count(NPC_COUNT as u64, SECTION, "entry-count")?;
                let mut entries = Vec::with_capacity(entry_count as usize);
                for _ in 0..entry_count {
                    let npc = dec.read_u64()?;
                    if npc >= NPC_COUNT as u64 {
                        return Err(SaveError::Integrity("region-director-save/npc-bound"));
                    }
                    let seq = dec.read_u64()?;
                    let represented_through = dec.read_tick()?;
                    let cursor = dec.read_u64()?;
                    let x = dec.read_u8()?;
                    let y = dec.read_u8()?;
                    if (x, y) != POSITION_DISCARDED && (x >= REGION_GRID || y >= REGION_GRID) {
                        return Err(SaveError::Integrity("region-director-save/position-bounds"));
                    }
                    let has_signal = read_flag(dec, SECTION)?;
                    entries.push(RegionEntry {
                        npc: npc as usize,
                        event: EventId { seq },
                        represented_through,
                        cursor,
                        position: (x, y),
                        has_signal,
                    });
                }
                Some(RegionCheckpoint {
                    dormant_since,
                    from_level,
                    entries,
                })
            } else {
                None
            };
            let last_method = match dec.read_u8()? {
                0 => None,
                1 => Some(ReconstructionMethod::Replay),
                2 => Some(ReconstructionMethod::Synthesis),
                discriminant => {
                    return Err(SaveError::Enum {
                        section: SECTION,
                        discriminant: u64::from(discriminant),
                    });
                }
            };
            let reconstructed_activities = dec.read_u64()?;
            let last_dormancy_span = dec.read_u64()?;
            states.push(RegionState {
                level,
                episode,
                checkpoint,
                last_method,
                reconstructed_activities,
                last_dormancy_span,
            });
        }
        // The capture ledger: pending is bounded by NPC_COUNT (each NPC
        // holds at most one in-flight capture — its current episode's);
        // applied grows across episodes, so it takes the general
        // collection bound. Both require strict-ascending canonical
        // order (the BTree iteration's form). The section's in-band
        // schema version: v3 carries the per-identity applied amounts
        // (v2's ids-only form never shipped in an accepted build —
        // both versions named here, the walk reads v3 only).
        match dec.read_u8()? {
            3 => {}
            discriminant => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(discriminant),
                });
            }
        }
        let pending_count = dec.read_count(NPC_COUNT as u64, SECTION, "pending-count")?;
        let mut pending = BTreeMap::new();
        let mut prev: Option<ConsequenceId> = None;
        for _ in 0..pending_count {
            let id = read_consequence_id(dec)?;
            let delta = dec.read_u64()?;
            if let Some(prior) = prev
                && id <= prior
            {
                return Err(SaveError::Order { section: SECTION });
            }
            prev = Some(id);
            pending.insert(id, delta);
        }
        let applied_count = dec.read_count(MAX_COLLECTION_COUNT, SECTION, "applied-count")?;
        let mut applied = BTreeMap::new();
        let mut prev: Option<ConsequenceId> = None;
        for _ in 0..applied_count {
            let id = read_consequence_id(dec)?;
            let amount = dec.read_u64()?;
            if let Some(prior) = prev
                && id <= prior
            {
                return Err(SaveError::Order { section: SECTION });
            }
            prev = Some(id);
            applied.insert(id, amount);
        }
        let mut director = RegionDirector::new(config)
            .map_err(|_| SaveError::Integrity("region-director-save/lod-config"))?;
        director.states = states;
        director.ledger = CaptureLedger { pending, applied };
        Ok(director)
    }
}

/// Reads a LOD level discriminant (fail-closed on unknown values).
pub(crate) fn read_lod_level(
    dec: &mut Decoder,
    section: &'static str,
) -> Result<LodLevel, SaveError> {
    match dec.read_u8()? {
        0 => Ok(LodLevel::Full),
        1 => Ok(LodLevel::Simplified),
        2 => Ok(LodLevel::EventDriven),
        3 => Ok(LodLevel::Dormant),
        discriminant => Err(SaveError::Enum {
            section,
            discriminant: u64::from(discriminant),
        }),
    }
}

/// Reads a configuration float — finite only (the decision path
/// computes distances and thresholds; a NaN or infinite value is
/// corruption, never a configuration).
fn read_finite_f64(dec: &mut Decoder) -> Result<f64, SaveError> {
    let value = f64::from_bits(dec.read_u64()?);
    if !value.is_finite() {
        return Err(SaveError::Integrity("region-director-save/finite-f64"));
    }
    Ok(value)
}

/// Reads a boolean flag (fail-closed on unknown discriminants).
pub(crate) fn read_flag(dec: &mut Decoder, section: &'static str) -> Result<bool, SaveError> {
    match dec.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        discriminant => Err(SaveError::Enum {
            section,
            discriminant: u64::from(discriminant),
        }),
    }
}

/// Reads one structured ledger identity, bounds-checked against this
/// workload's region and NPC counts.
fn read_consequence_id(dec: &mut Decoder) -> Result<ConsequenceId, SaveError> {
    let region = dec.read_u32()?;
    let episode = dec.read_u64()?;
    let npc = dec.read_u32()?;
    if region as usize >= REGION_COUNT || npc as usize >= NPC_COUNT {
        return Err(SaveError::Integrity("region-director-save/identity-bounds"));
    }
    Ok(ConsequenceId::new(region, episode, npc))
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

pub(crate) fn lod_byte(level: LodLevel) -> u8 {
    match level {
        LodLevel::Full => 0,
        LodLevel::Simplified => 1,
        LodLevel::EventDriven => 2,
        LodLevel::Dormant => 3,
    }
}
