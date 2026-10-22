//! Stage 3 NPC scheduling workload — application data over the engine
//! spine (game policy by *placement*, `docs/TECH-ARCHITECTURE.md` §1 P5;
//! Stage 3 authorization §2: schedules are game data; the spine is the
//! reusable mechanism, never re-implemented here).
//!
//! 1,000 stable NPC identities, each with a chained activity schedule:
//! every execution advances that NPC's logical activity step and schedules
//! the next activity as an execution-relative follow-up (which inherits
//! the executing event's near/far lane — the chain stays in its region's
//! lane). Region identifiers are metadata only: no region activation,
//! streaming, or LOD in Stage 3.

use engine_core::{
    Decoder, Encoder, Event, EventClass, EventSpec, ExecCtx, Field, Fingerprinter, LodLevel,
    SaveError, SaveWorld, Schedule, Spine, Tick, World,
};

/// Eight logical region identifiers (Stage 3 metadata for later stages).
pub const REGION_COUNT: usize = 8;

/// The Stage 4 workload: 1,000 stable NPC identities.
pub const NPC_COUNT: usize = 1000;

/// Real-execution milestone period: every MILESTONE_PERIOD-th actual
/// activity is an irreversible consequence. Milestones arise ONLY from
/// real event execution — never from reconstruction (reviewer priority
/// A: reconstructed progression cannot fabricate irreversible
/// consequences).
pub const MILESTONE_PERIOD: u64 = 16;

/// Side length of the per-NPC region-local position grid (bounds and
/// travel-reachability invariants; derived render proxies).
pub const REGION_GRID: u8 = 16;

/// How the workload handler chains activities (Stage 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionMode {
    /// Stage 3 behavior, character-for-character: period + jitter
    /// delays, lane-inheriting follow-ups, payload activity + 1. The
    /// frozen `npc-world/1` baselines depend on this path staying
    /// untouched.
    Legacy,
    /// Stage 4 region-LOD behavior: per-region cadence factors, explicit
    /// -lane follow-ups under the region's current near/far
    /// classification, routine-cursor payload re-basing, position
    /// advances, and real-execution milestones.
    Lod,
}

/// Workload configuration — all policy, all configurable (Stage 3 §2,
/// Stage 4 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpcConfig {
    /// Deterministic workload seed. Identity assignment is stable by id;
    /// the seed drives schedule offsets and activity jitter.
    pub seed: u64,
    /// Regions whose events are scheduled into the near lane — the
    /// configurable near/far classification criterion (§2.4 policy, E-10).
    pub near_regions: [bool; REGION_COUNT],
    /// Base activity period in ticks: each NPC chains one activity per
    /// period, with seeded jitter that also desynchronizes schedule
    /// synchrony clusters (E-10: budgets are sized to cluster peaks).
    pub activity_period: u64,
    /// Stage 4 demo policy: the cadence multiplier at Simplified LOD
    /// (coarser, less frequent updates — meaningfully less work).
    pub simplify_factor: u64,
    /// Stage 4 demo policy: the cadence multiplier at EventDriven LOD —
    /// the commitment window between long-cadence wake-ups (records and
    /// scheduled commitments only).
    pub commitment_window: u64,
    /// Stage 4: enable the region-LOD handler path. `false` (the
    /// default) keeps the Stage 3 Legacy path, so every historical
    /// fingerprint stays byte-identical.
    pub region_lods_enabled: bool,
}

impl Default for NpcConfig {
    fn default() -> Self {
        // Two of eight regions near-field — the minority under far-field
        // pressure, exercising the reservation's anti-starvation purpose.
        let mut near_regions = [false; REGION_COUNT];
        near_regions[0] = true;
        near_regions[1] = true;
        NpcConfig {
            seed: 1337,
            near_regions,
            activity_period: 6,
            simplify_factor: 4,
            commitment_window: 12,
            region_lods_enabled: false,
        }
    }
}

impl NpcConfig {
    /// Cadence multiplier at a LOD level (demo policy, configurable):
    /// Full = every period, Simplified = period × simplify factor,
    /// EventDriven = period × commitment window. Dormant never executes
    /// (its obligations are suspended), so the value is defensive.
    pub fn cadence_factor(&self, lod: LodLevel) -> u64 {
        match lod {
            LodLevel::Full => 1,
            LodLevel::Simplified => self.simplify_factor.max(1),
            LodLevel::EventDriven | LodLevel::Dormant => self.commitment_window.max(1),
        }
    }

    /// Near/far classification for a region at a LOD level (Stage 4
    /// demo policy, application-side by contract): player-proximate
    /// levels classify Near — the reservation protects them — while
    /// far-field levels classify Far.
    pub fn lod_field(&self, lod: LodLevel) -> Field {
        match lod {
            LodLevel::Full | LodLevel::Simplified => Field::Near,
            LodLevel::EventDriven | LodLevel::Dormant => Field::Far,
        }
    }
}

/// Encodes NPC identity and activity into the event payload (both
/// directions lossless for `npc < 2^16` and `activity < 2^16`; the
/// 16-bit activity field bounds a chain at ~65k activities — about ten
/// hours at the 10 Hz demo rate with period 6, ample for Stage 3).
pub fn payload(npc: usize, activity: u64) -> u64 {
    ((npc as u64) << 16) | (activity & 0xFFFF)
}

pub fn payload_npc(payload: u64) -> usize {
    (payload >> 16) as usize
}

pub fn payload_activity(payload: u64) -> u64 {
    payload & 0xFFFF
}

/// Stage 4 routine-chain type tag: Lod-mode chain obligations carry an
/// explicit marker, so a held obligation is treated as an NPC routine
/// chain only when the payload says so (Batch 2 corrective finding A /
/// reviewer addition) — an obligation scheduled into a dormant region
/// after the checkpoint exists resumes with its own identity and is
/// never mistaken for a routine chain, and no foreign payload can
/// advance NPC state under the Legacy layout's decode.
pub const ROUTINE_MAGIC: u64 = 0x5E47;

/// Encodes a Lod-mode routine-chain payload: magic(16) | npc(16) |
/// cursor(32). The 32-bit cursor bounds a chain at ~4 billion
/// activities — unreachable at demo cadences.
pub fn routine_payload(npc: usize, cursor: u64) -> u64 {
    (ROUTINE_MAGIC << 48) | ((npc as u64) << 32) | (cursor & 0xFFFF_FFFF)
}

/// Decodes a routine-chain payload: `Some((npc, cursor))` only when the
/// payload carries the routine type tag AND the identity field is a
/// valid NPC. Foreign payloads — including ones that would decode as
/// an NPC under the Legacy layout — stay foreign.
pub fn routine_payload_parts(payload: u64) -> Option<(usize, u64)> {
    if payload >> 48 != ROUTINE_MAGIC {
        return None;
    }
    let npc = ((payload >> 32) & 0xFFFF) as usize;
    if npc >= NPC_COUNT {
        return None;
    }
    Some((npc, payload & 0xFFFF_FFFF))
}

/// Sentinel for a dormant NPC's discarded routine cursor: deactivation
/// discards the reconstructible representation (corrective finding A)
/// and reconstruction restores it from the region checkpoint.
pub const ROUTINE_CURSOR_DISCARDED: u64 = u64::MAX;

/// Sentinel for a dormant NPC's discarded grid position.
pub const POSITION_DISCARDED: (u8, u8) = (u8::MAX, u8::MAX);

/// Sentinel for a dormant NPC's discarded represented-through boundary
/// (Batch 2 corrective finding D).
pub const REPRESENTED_THROUGH_DISCARDED: Tick = Tick(u64::MAX);

/// splitmix64 finalizer — a fully specified, stable mixer (no `std`
/// hasher nondeterminism reaches fingerprinted state).
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The NPC world: per-NPC activity steps are the authoritative logical
/// state; diagnostics derive from them read-only.
pub struct NpcWorld {
    config: NpcConfig,
    /// Executed activity count per NPC, indexed by stable identity —
    /// ACTUAL executions only (reviewer priority A): reconstruction
    /// never increments this counter.
    steps: Vec<u64>,
    /// Stage 4 routine progression cursor per NPC: the index of the next
    /// activity the routine would perform — advanced by real execution
    /// AND by labeled reconstruction. Separate from `steps` by contract;
    /// equivalently `cursor == steps` whenever no reconstruction has
    /// occurred, which is what keeps the Legacy chain coherent.
    routine_cursor: Vec<u64>,
    /// Per-NPC position on the region-local grid (x, y in 0..REGION_GRID)
    /// — bounds- and travel-reachability-checked state, and the source
    /// for derived render proxies.
    positions: Vec<(u8, u8)>,
    /// Per-NPC represented-through boundary (Batch 2 corrective finding
    /// D): the logical time through which this NPC's routine progression
    /// is represented — the coherent anchor a later reconstruction
    /// counts FROM, so an already-represented interval is never
    /// reconstructed again. Updated by span reconstruction (replay's
    /// exact stopping point — the last represented activity's time;
    /// synthesis's closed-form boundary). Whenever the pending
    /// obligation's payload cursor equals the world cursor (the state
    /// was last advanced by real execution, which re-bases the chain),
    /// the obligation's own due is the boundary and this value is not
    /// consulted; deactivation picks whichever anchor matches the
    /// captured state.
    represented_through: Vec<Tick>,
    /// Real-execution milestones per NPC: every MILESTONE_PERIOD-th
    /// actual activity. Irreversible consequences — never fabricated by
    /// reconstruction.
    milestones: Vec<u64>,
    /// Stage 4 uncommitted consequential signals per NPC: every
    /// MILESTONE_PERIOD-th real activity accrues one pending signal —
    /// the demo's consequential application state. SCRATCH by contract:
    /// deactivation captures every uncommitted signal into the region
    /// director's in-memory ledger (exactly-once keyed application)
    /// BEFORE this scratch is cleared — the E-07/I1
    /// capture-before-discard miniature (Batch 2 corrective finding A).
    /// Never touched by reconstruction.
    pending_signals: Vec<u64>,
    /// Committed consequential signals per NPC — durable state grown
    /// ONLY by exactly-once ledger application at reactivation. Never
    /// touched by reconstruction; replay and synthesis cannot fabricate
    /// a credit.
    credits: Vec<u64>,
    /// The director's current LOD per region — the handler's cadence and
    /// classification view. The director (region state) owns the
    /// authoritative levels; this copy is what execution actually
    /// consumed, and is fingerprinted with the routine state.
    region_lods: [LodLevel; REGION_COUNT],
}

impl NpcWorld {
    pub fn new(config: NpcConfig) -> Self {
        let positions = (0..NPC_COUNT)
            .map(|npc| {
                let hash = mix64(config.seed ^ ((npc as u64) << 32) ^ 0xB0B0_0000_0000_0000);
                (
                    (hash % u64::from(REGION_GRID)) as u8,
                    ((hash >> 32) % u64::from(REGION_GRID)) as u8,
                )
            })
            .collect();
        NpcWorld {
            config,
            steps: vec![0; NPC_COUNT],
            routine_cursor: vec![0; NPC_COUNT],
            positions,
            represented_through: vec![Tick(0); NPC_COUNT],
            milestones: vec![0; NPC_COUNT],
            pending_signals: vec![0; NPC_COUNT],
            credits: vec![0; NPC_COUNT],
            region_lods: [LodLevel::Full; REGION_COUNT],
        }
    }

    pub fn config(&self) -> &NpcConfig {
        &self.config
    }

    /// Total executed activities across all NPCs.
    pub fn total_steps(&self) -> u64 {
        self.steps.iter().sum()
    }

    /// Per-NPC activity steps, indexed by identity (read-only view).
    pub fn steps(&self) -> &[u64] {
        &self.steps
    }

    /// Per-NPC routine progression cursors (read-only view).
    pub fn routine_cursor(&self) -> &[u64] {
        &self.routine_cursor
    }

    /// Per-NPC grid positions (read-only view).
    pub fn positions(&self) -> &[(u8, u8)] {
        &self.positions
    }

    /// Per-NPC represented-through boundaries (read-only view) — the
    /// coherent reconstruction anchors captured into checkpoints.
    pub fn represented_through(&self) -> &[Tick] {
        &self.represented_through
    }

    /// Per-NPC real-execution milestones (read-only view).
    pub fn milestones(&self) -> &[u64] {
        &self.milestones
    }

    /// Per-NPC uncommitted consequential signals (read-only view) —
    /// the scratch captured into the region ledger at deactivation.
    pub fn pending_signals(&self) -> &[u64] {
        &self.pending_signals
    }

    /// Per-NPC committed consequential signals (read-only view) —
    /// durable state grown only by exactly-once ledger application.
    pub fn credits(&self) -> &[u64] {
        &self.credits
    }

    /// The handler's current LOD view per region.
    pub fn region_lods(&self) -> [LodLevel; REGION_COUNT] {
        self.region_lods
    }

    /// Sets one region's LOD (the director's tick-boundary update —
    /// never mid-tick).
    pub fn set_region_lod(&mut self, region: usize, lod: LodLevel) {
        self.region_lods[region] = lod;
    }

    /// The lane for a region under the configured classification.
    pub fn region_field(&self, region: usize) -> Field {
        if self.config.near_regions[region] {
            Field::Near
        } else {
            Field::Far
        }
    }

    /// Discards a dormant region's reconstructible routine
    /// representation (Batch 2 corrective findings A and D): the
    /// routine cursor, grid position, and represented-through boundary
    /// become sentinels — the representation is GONE, not merely
    /// flagged — and the uncommitted signal scratch is cleared AFTER
    /// the director captured its consequences into the ledger. Steps,
    /// milestones, and credits — authoritative consequential state —
    /// are retained untouched.
    pub fn discard_region_routine(&mut self, region: usize) {
        for npc in 0..NPC_COUNT {
            if npc % REGION_COUNT == region {
                self.routine_cursor[npc] = ROUTINE_CURSOR_DISCARDED;
                self.positions[npc] = POSITION_DISCARDED;
                self.represented_through[npc] = REPRESENTED_THROUGH_DISCARDED;
                self.pending_signals[npc] = 0;
            }
        }
    }

    /// Restores one NPC's routine cursor, grid position, and
    /// represented-through boundary from the region checkpoint (the
    /// reconstructible state captured at deactivation) — the seed the
    /// span reconstruction advances from at reactivation, counting only
    /// the previously unrepresented interval (corrective finding D).
    pub fn restore_routine(
        &mut self,
        npc: usize,
        cursor: u64,
        position: (u8, u8),
        represented_through: Tick,
    ) {
        self.routine_cursor[npc] = cursor;
        self.positions[npc] = position;
        self.represented_through[npc] = represented_through;
    }

    /// Commits one captured signal delta — the reactivation-side
    /// application of a captured consequence. Only the director calls
    /// this, and only with a capture-ledger `Applied` outcome, so each
    /// delta commits exactly once.
    pub fn apply_captured_signal(&mut self, npc: usize, delta: u64) {
        self.credits[npc] += delta;
    }

    /// Exact step-by-step replay of a dormant span (reviewer priority
    /// A/C, Batch 2 corrective finding D): recomputes the seeded jitter
    /// chain activity by activity AFTER `from_due` (the coherent
    /// anchor — the pending obligation's own activity is performed by
    /// real execution, never reconstructed), advancing the routine
    /// cursor and position walk exactly as live execution would have.
    /// Cost is proportional to the reconstructed activity count
    /// (E-09's replay). NEVER touches `steps`, milestones, credits, or
    /// the execution ledger — reconstruction is labeled state, not
    /// history. Records the loop's exact stopping time as the
    /// represented-through boundary, so a later reconstruction resumes
    /// from there and this span is never re-represented. Returns the
    /// reconstructed activity count.
    pub fn replay_routine_span(
        &mut self,
        npc: usize,
        from_due: Tick,
        now: Tick,
        cadence: u64,
    ) -> u64 {
        let period = self.config.activity_period * cadence.max(1);
        let activity = self.routine_cursor[npc];
        let mut time = from_due.as_u64();
        let mut count = 0u64;
        loop {
            let jitter = mix64(self.config.seed ^ ((npc as u64) << 32) ^ (activity + count)) % 2;
            let delay = period + jitter;
            if time.saturating_add(delay) > now.as_u64() {
                break;
            }
            time += delay;
            count += 1;
        }
        // The boundary is the last represented activity's time (the
        // loop's stopping point); a zero-count replay leaves the anchor
        // at `from_due`, unchanged.
        self.represented_through[npc] = Tick(time);
        self.apply_routine_advance(npc, count);
        count
    }

    /// Closed-form synthesis of a dormant span (reviewer priority A,
    /// Batch 2 corrective finding D): the approximate activity count
    /// from elapsed time over the mean cadence, with a bounded-drift
    /// position displacement (at most one cell per activity —
    /// travel-reachable by construction). O(1). NEVER touches `steps`,
    /// milestones, credits, or the ledger; the caller labels the span
    /// as synthesized. Records the closed-form represented-through
    /// boundary (`from_due + missed × period`), so a later
    /// reconstruction resumes from there and this span is never
    /// re-represented. Returns the synthesized count.
    pub fn synthesize_routine_span(
        &mut self,
        npc: usize,
        from_due: Tick,
        now: Tick,
        cadence: u64,
    ) -> u64 {
        let period = (self.config.activity_period * cadence.max(1)).max(1);
        let elapsed = now.as_u64().saturating_sub(from_due.as_u64());
        let missed = elapsed / period;
        let cursor = self.routine_cursor[npc];
        // Average-drift displacement: half the activities' worth of cells
        // in a seeded direction — bounded by `missed`, so reachability
        // holds by construction.
        let direction = mix64(self.config.seed ^ ((npc as u64) << 32) ^ cursor ^ 0x5151) % 4;
        let travel = (missed / 2) as i16;
        let (x, y) = self.positions[npc];
        let (dx, dy) = match direction {
            0 => (travel, 0),
            1 => (-travel, 0),
            2 => (0, travel),
            _ => (0, -travel),
        };
        let nx = (i16::from(x) + dx).clamp(0, i16::from(REGION_GRID) - 1) as u8;
        let ny = (i16::from(y) + dy).clamp(0, i16::from(REGION_GRID) - 1) as u8;
        self.positions[npc] = (nx, ny);
        self.routine_cursor[npc] += missed;
        self.represented_through[npc] = Tick(from_due.as_u64() + missed * period);
        missed
    }

    /// Advances the routine cursor and position walk by `count`
    /// activities (the per-step replay rule: one seeded cell per
    /// activity, clamped to the region grid — the bounds invariant).
    fn apply_routine_advance(&mut self, npc: usize, count: u64) {
        for _ in 0..count {
            self.routine_cursor[npc] += 1;
            self.advance_position(npc);
        }
    }

    /// One seeded position step on the region-local grid, clamped to
    /// bounds (the travel-reachability unit: at most one cell per
    /// activity).
    fn advance_position(&mut self, npc: usize) {
        let cursor = self.routine_cursor[npc];
        let direction = mix64(self.config.seed ^ ((npc as u64) << 32) ^ cursor ^ 0x5050) % 4;
        let (x, y) = self.positions[npc];
        let (nx, ny) = match direction {
            0 => (i16::from(x) + 1, i16::from(y)),
            1 => (i16::from(x) - 1, i16::from(y)),
            2 => (i16::from(x), i16::from(y) + 1),
            _ => (i16::from(x), i16::from(y) - 1),
        };
        let nx = nx.clamp(0, i16::from(REGION_GRID) - 1) as u8;
        let ny = ny.clamp(0, i16::from(REGION_GRID) - 1) as u8;
        self.positions[npc] = (nx, ny);
    }

    /// Stage 4 authoritative workload state beyond the frozen Stage 3
    /// domain: routine cursor (with discard sentinels while dormant),
    /// grid positions, the represented-through boundaries, milestones,
    /// the region-LOD view execution actually consumed, and the
    /// consequential signal state — uncommitted scratch plus committed
    /// credits. Domain version 3 (Batch 2 corrective finding D): adds
    /// the represented-through boundaries; versions 1–2 never reached
    /// main. Hashed only by the Stage 4 combined fingerprint; the
    /// Stage 3 domains never see it.
    pub fn spatial_fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-slice/npc-spatial/3");
        fp.write_u64(self.config.seed);
        fp.write_usize(self.routine_cursor.len());
        for cursor in &self.routine_cursor {
            fp.write_u64(*cursor);
        }
        for position in &self.positions {
            fp.write_u8(position.0);
            fp.write_u8(position.1);
        }
        for boundary in &self.represented_through {
            fp.write_tick(*boundary);
        }
        for milestone in &self.milestones {
            fp.write_u64(*milestone);
        }
        for signal in &self.pending_signals {
            fp.write_u64(*signal);
        }
        for credit in &self.credits {
            fp.write_u64(*credit);
        }
        for lod in &self.region_lods {
            fp.write_u8(match lod {
                LodLevel::Full => 0,
                LodLevel::Simplified => 1,
                LodLevel::EventDriven => 2,
                LodLevel::Dormant => 3,
            });
        }
    }
}

/// Installs the initial schedule for every NPC identity: one event per
/// NPC, due within the first two activity periods (seeded spread), in
/// the region's lane. The chain then self-sustains through follow-ups.
pub fn install_initial_schedules(spine: &mut Spine<NpcWorld>, config: &NpcConfig) {
    // Stage 4: in Lod mode the install classifies and spreads under the
    // region-LOD view the director set before installation (the world
    // defaults to Full everywhere when no director has run — the same
    // all-active start Stage 3 used).
    let lods = spine.world().region_lods();
    for npc in 0..NPC_COUNT {
        let region = npc % REGION_COUNT;
        if config.region_lods_enabled {
            let lod = lods[region];
            let field = config.lod_field(lod);
            let spread = (config.activity_period * 2 * config.cadence_factor(lod)).max(1);
            let offset = mix64(config.seed ^ ((npc as u64) << 32)) % spread;
            spine
                .schedule(
                    EventSpec::new(
                        EventClass::new(region as u16),
                        Schedule::AtLogicalTime {
                            due: Tick(1 + offset),
                        },
                        routine_payload(npc, 0),
                    )
                    .in_field(field),
                )
                .expect("initial NPC schedule is dependency-free");
        } else {
            // Stage 3 Legacy path — untouched (frozen baselines).
            let field = if config.near_regions[region] {
                Field::Near
            } else {
                Field::Far
            };
            let spread = (config.activity_period * 2).max(1);
            let offset = mix64(config.seed ^ ((npc as u64) << 32)) % spread;
            spine
                .schedule(
                    EventSpec::new(
                        EventClass::new(region as u16),
                        Schedule::AtLogicalTime {
                            due: Tick(1 + offset),
                        },
                        payload(npc, 0),
                    )
                    .in_field(field),
                )
                .expect("initial NPC schedule is dependency-free");
        }
    }
}

impl World for NpcWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        if !self.config.region_lods_enabled {
            // Stage 3 Legacy path, character-for-character — the frozen
            // npc-world/1 baselines hash this exact behavior. Payloads
            // outside the identity space (headless burst/diagnostics
            // injections) are not NPC activities: the spine counts them
            // in its ledger; the workload ignores them.
            let npc = payload_npc(event.payload);
            if npc >= NPC_COUNT {
                return;
            }
            let activity = payload_activity(event.payload);
            self.steps[npc] += 1;
            // Chain the next activity: execution-relative (anchored to actual
            // execution), same region class, same lane (follow-ups inherit
            // the executing event's field). Jitter derives from logical data
            // only — seed, identity, activity — never wall-clock time.
            let jitter = mix64(self.config.seed ^ ((npc as u64) << 32) ^ activity) % 2;
            let delay = self.config.activity_period + jitter;
            ctx.follow_up(delay, event.class, payload(npc, activity + 1), Vec::new());
            return;
        }
        // Stage 4 region-LOD path: routine chains are EXPLICITLY TYPED
        // (corrective finding A) — only payloads carrying the routine
        // tag advance NPC state; foreign payloads are counted in the
        // spine's ledger and ignored here, never mistaken for a chain.
        let Some((npc, cursor)) = routine_payload_parts(event.payload) else {
            return;
        };
        // Class↔region consistency (Batch 3 defensive check): this
        // workload's application policy maps an event's class to the
        // NPC's home region (npc mod 8). A malformed routine-tagged
        // event whose identity belongs to a different region is
        // ignored by the workload — it can never advance another
        // region's NPC state (the spine still counts the execution in
        // its ledger).
        if npc % REGION_COUNT != usize::from(event.class.0) {
            return;
        }
        // `steps` counts actual executions only; the routine cursor —
        // advanced here AND by labeled reconstruction — re-bases the
        // chained payload, so a reconstructed span never double-counts
        // and a resumed obligation performs the next activity after the
        // reconstructed state. Milestones — irreversible consequences —
        // arise only from this real-execution counter, and each crossing
        // accrues one PENDING consequential signal: scratch until the
        // region's deactivation captures it into the ledger and its
        // reactivation commits it exactly once.
        self.steps[npc] += 1;
        if self.steps[npc].is_multiple_of(MILESTONE_PERIOD) {
            self.milestones[npc] += 1;
            self.pending_signals[npc] += 1;
        }
        self.routine_cursor[npc] += 1;
        self.advance_position(npc);
        let region = npc % REGION_COUNT;
        let lod = self.region_lods[region];
        let cadence = self.config.cadence_factor(lod);
        // Jitter derives from logical data only — seed, identity,
        // cursor — never wall-clock time.
        let jitter = mix64(self.config.seed ^ ((npc as u64) << 32) ^ cursor) % 2;
        let delay = self.config.activity_period * cadence + jitter;
        let field = self.config.lod_field(lod);
        ctx.follow_up_in_field(
            field,
            delay,
            event.class,
            routine_payload(npc, self.routine_cursor[npc]),
            Vec::new(),
        );
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        // FROZEN Stage 3 domain "engine-slice/npc-world/1": the new
        // Stage 4 state (routine cursor, positions, milestones, region
        // LODs) never enters here — it lives in
        // [`Self::spatial_fingerprint`] under a new versioned domain, so
        // every historical fingerprint stays byte-identical.
        fp.write_str("engine-slice/npc-world/1");
        fp.write_u64(self.config.seed);
        fp.write_u64(self.config.activity_period);
        let mut near_mask = 0u64;
        for (index, near) in self.config.near_regions.iter().enumerate() {
            if *near {
                near_mask |= 1 << index;
            }
        }
        fp.write_u64(near_mask);
        fp.write_usize(self.steps.len());
        for step in &self.steps {
            fp.write_u64(*step);
        }
    }
}

impl SaveWorld for NpcWorld {
    const SAVE_DOMAIN: &'static str = "engine-slice/npc-world-save/1";

    fn save(&self, enc: &mut Encoder) {
        // The workload configuration first — behavioral identity: a
        // restored world must chain, jitter, and reconstruct exactly
        // as the saved one would, so every value execution consults
        // is on the wire (the Batch 4 authoritative-state codec; the
        // frozen `npc-world/1` fingerprint domain is untouched and
        // never sees this section).
        enc.write_u64(self.config.seed);
        let mut near_mask = 0u64;
        for (index, near) in self.config.near_regions.iter().enumerate() {
            if *near {
                near_mask |= 1 << index;
            }
        }
        enc.write_u64(near_mask);
        enc.write_u64(self.config.activity_period);
        enc.write_u64(self.config.simplify_factor);
        enc.write_u64(self.config.commitment_window);
        enc.write_u8(u8::from(self.config.region_lods_enabled));
        // The authoritative per-NPC state: dense arrays indexed by the
        // stable identity set — ONE count governs every array (the
        // identity set is fixed; a different count is an incompatible
        // workload, rejected at restore, never a resized restore).
        enc.write_count(NPC_COUNT);
        for step in &self.steps {
            enc.write_u64(*step);
        }
        for cursor in &self.routine_cursor {
            enc.write_u64(*cursor);
        }
        for position in &self.positions {
            enc.write_u8(position.0);
            enc.write_u8(position.1);
        }
        for boundary in &self.represented_through {
            enc.write_tick(*boundary);
        }
        for milestone in &self.milestones {
            enc.write_u64(*milestone);
        }
        for signal in &self.pending_signals {
            enc.write_u64(*signal);
        }
        for credit in &self.credits {
            enc.write_u64(*credit);
        }
        for lod in &self.region_lods {
            enc.write_u8(match lod {
                LodLevel::Full => 0,
                LodLevel::Simplified => 1,
                LodLevel::EventDriven => 2,
                LodLevel::Dormant => 3,
            });
        }
    }

    fn restore(dec: &mut Decoder) -> Result<Self, SaveError> {
        const SECTION: &str = "npc-world-save";
        let seed = dec.read_u64()?;
        let near_mask = dec.read_u64()?;
        let activity_period = dec.read_u64()?;
        let simplify_factor = dec.read_u64()?;
        let commitment_window = dec.read_u64()?;
        let region_lods_enabled = match dec.read_u8()? {
            0 => false,
            1 => true,
            discriminant => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(discriminant),
                });
            }
        };
        // The fixed identity set: the count is bounded by the
        // documented workload size and the remaining bytes before any
        // allocation trusts it, and must equal NPC_COUNT exactly — a
        // different identity set is an incompatible workload, never a
        // silently resized restore.
        let npc_count = dec.read_count(NPC_COUNT as u64, SECTION, "npc-count")?;
        if npc_count != NPC_COUNT as u64 {
            return Err(SaveError::Integrity("npc-world-save/npc-count"));
        }
        let mut steps = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            steps.push(dec.read_u64()?);
        }
        let mut routine_cursor = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            routine_cursor.push(dec.read_u64()?);
        }
        // Grid positions carry the dormant-discard sentinel
        // (`POSITION_DISCARDED` — the reconstructible representation is
        // GONE, not merely flagged); every other position must lie on
        // the region-local grid (the bounds invariant).
        let mut positions = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            let x = dec.read_u8()?;
            let y = dec.read_u8()?;
            if (x, y) != POSITION_DISCARDED && (x >= REGION_GRID || y >= REGION_GRID) {
                return Err(SaveError::Integrity("npc-world-save/position-bounds"));
            }
            positions.push((x, y));
        }
        let mut represented_through = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            represented_through.push(dec.read_tick()?);
        }
        let mut milestones = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            milestones.push(dec.read_u64()?);
        }
        let mut pending_signals = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            pending_signals.push(dec.read_u64()?);
        }
        let mut credits = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            credits.push(dec.read_u64()?);
        }
        let mut region_lods = [LodLevel::Full; REGION_COUNT];
        for lod in &mut region_lods {
            *lod = match dec.read_u8()? {
                0 => LodLevel::Full,
                1 => LodLevel::Simplified,
                2 => LodLevel::EventDriven,
                3 => LodLevel::Dormant,
                discriminant => {
                    return Err(SaveError::Enum {
                        section: SECTION,
                        discriminant: u64::from(discriminant),
                    });
                }
            };
        }
        // The near-region mask is a REGION_COUNT-bit mask — set bits
        // above it are not this workload's configuration.
        if near_mask >> REGION_COUNT != 0 {
            return Err(SaveError::Integrity("npc-world-save/near-mask"));
        }
        let mut near_regions = [false; REGION_COUNT];
        for (index, near) in near_regions.iter_mut().enumerate() {
            *near = near_mask & (1 << index) != 0;
        }
        let config = NpcConfig {
            seed,
            near_regions,
            activity_period,
            simplify_factor,
            commitment_window,
            region_lods_enabled,
        };
        Ok(NpcWorld {
            config,
            steps,
            routine_cursor,
            positions,
            represented_through,
            milestones,
            pending_signals,
            credits,
            region_lods,
        })
    }
}
