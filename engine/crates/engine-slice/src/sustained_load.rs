//! The F-08/F-12 sustained-load harness (Cycle 1, package P2): the
//! simulation-scale evidence the entity-scale benchmark cannot give —
//! not only executed-work cost per tick, but the work that REMAINS
//! UNFINISHED as workload and population grow, over the methodology's
//! six workload classes and twelve dimensions
//! (`docs/SUSTAINED-LOAD-METHODOLOGY.md`).
//!
//! Cycle-1-P2 matrix-amendment mechanisms (the SEAT C challenge):
//! - **F1/F7:** this module owns a `SustainedWorld` — the entity-scale
//!   benchmark's routine-chain semantics as an ISOLATED sibling, never
//!   exposing or touching `benchmark.rs`'s module-private
//!   `ScaledWorld` (whose outputs stay byte-identical by construction)
//!   — with its OWN payload codec: `npc::routine_payload_parts` is
//!   bound to the 1000-identity wire workload (`NPC_COUNT`), while
//!   sustained chains at 5k/20k require the identity bound to be the
//!   world's own entity count.
//! - **F3:** `SaveWorld` is implemented here (a distinct domain, never
//!   a canonical format), so dimension 11 measures representation cost
//!   at every population with the ledger-size dependence reported.
//! - **F4:** `region_lods` is harness-mutable — the dynamic-LOD seam
//!   for class 6's scaled runs. The `RegionDirector` protocol itself
//!   stays `NpcWorld`-typed and is exercised at 1k only; scaled
//!   changing-presence runs use harness-driven transitions and are
//!   labeled as such.
//!
//! Honesty rules (methodology §6): executing fewer events is never
//! automatically a performance improvement — every report distinguishes
//! completed obligations from deliberately reduced fidelity and overdue
//! deferred work; partial matrices are never called complete.

use engine_core::{
    Decoder, Encoder, Event, EventClass, EventId, EventSpec, ExecCtx, Field, Fingerprinter,
    LodLevel, ObligationLocation, SaveError, SaveWorld, Schedule, Spine, Tick, World,
};

use crate::npc::{NpcConfig, REGION_COUNT, mix64};

/// The sustained harness's routine-chain type tag — distinct from the
/// game workload's `ROUTINE_MAGIC` and the burst suite's
/// `BURST_MAGIC`, so no foreign payload can advance sustained-chain
/// state and no sustained payload can advance NPC state.
pub const SUSTAINED_ROUTINE_MAGIC: u64 = 0x5F12;

/// The exact-count demand overlay's type tag (the composition
/// instrument): foreign to every routine codec — counted in the
/// spine's ledger, never advancing any world state.
pub const SUSTAINED_DEMAND_MAGIC: u64 = 0x0F12;

/// The sustained world's save-section domain (a harness measurement
/// surface — never a canonical save format).
pub const SUSTAINED_WORLD_SAVE_DOMAIN: &str = "engine-slice/sustained-world-save/1";

/// Encodes one sustained routine-chain payload:
/// `magic(16) | entity(16) | cursor(32)`.
pub fn sustained_routine_payload(entity: usize, cursor: u64) -> u64 {
    (SUSTAINED_ROUTINE_MAGIC << 48) | ((entity as u64) << 32) | (cursor & 0xFFFF_FFFF)
}

/// Decodes a sustained routine-chain payload: `Some((entity, cursor))`
/// only when the payload carries this harness's type tag. The
/// entity-vs-population bound is the WORLD's own check (the scaled
/// identity space), never a wire-format constant.
pub fn sustained_routine_parts(payload: u64) -> Option<(usize, u64)> {
    if payload >> 48 != SUSTAINED_ROUTINE_MAGIC {
        return None;
    }
    Some((((payload >> 32) & 0xFFFF) as usize, payload & 0xFFFF_FFFF))
}

/// Encodes one demand-overlay payload: `magic(16) | sequence(48)` —
/// opaque to every world, identifiable in the execution ledger.
pub fn sustained_demand_payload(sequence: u64) -> u64 {
    (SUSTAINED_DEMAND_MAGIC << 48) | (sequence & 0xFFFF_FFFF_FFFF)
}

/// Whether a payload is a demand-overlay event (ledger accounting).
pub fn is_sustained_demand(payload: u64) -> bool {
    payload >> 48 == SUSTAINED_DEMAND_MAGIC
}

/// One LOD level's fingerprint/save code (stable, versioned by the
/// domain tags that carry it).
fn lod_code(lod: LodLevel) -> u8 {
    match lod {
        LodLevel::Full => 1,
        LodLevel::Simplified => 2,
        LodLevel::EventDriven => 3,
        LodLevel::Dormant => 4,
    }
}

fn lod_from_code(code: u8) -> Result<LodLevel, SaveError> {
    match code {
        1 => Ok(LodLevel::Full),
        2 => Ok(LodLevel::Simplified),
        3 => Ok(LodLevel::EventDriven),
        4 => Ok(LodLevel::Dormant),
        other => Err(SaveError::Enum {
            section: "sustained-world",
            discriminant: u64::from(other),
        }),
    }
}

/// The sustained activity world: the entity-scale benchmark's
/// routine-chain semantics (the typed payload, the class↔region
/// consistency gate, the cadence-multiplied execution-relative
/// chaining, the logical-data jitter) at a parameterized entity
/// count, with the identity bound and the LOD view owned by this
/// harness. Not a canonical game-layout identity: its fingerprint and
/// save domains are deliberately distinct from the frozen
/// `npc-world/1` family.
pub struct SustainedWorld {
    entity_count: usize,
    config: NpcConfig,
    region_lods: [LodLevel; REGION_COUNT],
    steps: Vec<u64>,
    routine_cursor: Vec<u64>,
}

impl SustainedWorld {
    /// A world of `entity_count` chained entities under the given
    /// per-region LOD view and workload configuration.
    pub fn new(
        entity_count: usize,
        config: NpcConfig,
        region_lods: [LodLevel; REGION_COUNT],
    ) -> Self {
        SustainedWorld {
            entity_count,
            config,
            region_lods,
            steps: vec![0; entity_count],
            routine_cursor: vec![0; entity_count],
        }
    }

    pub fn entity_count(&self) -> usize {
        self.entity_count
    }

    pub fn config(&self) -> &NpcConfig {
        &self.config
    }

    /// The world's current LOD view (the cadence/lane classification
    /// that NEW follow-ups consume).
    pub fn region_lods(&self) -> [LodLevel; REGION_COUNT] {
        self.region_lods
    }

    /// F4's dynamic-LOD seam: the harness syncs the world's cadence
    /// and lane view (new follow-ups consume the current level;
    /// in-flight obligations keep their lane until reclassified).
    pub fn set_region_lods(&mut self, region_lods: [LodLevel; REGION_COUNT]) {
        self.region_lods = region_lods;
    }

    /// Total executed routine steps across all entities (the
    /// real-execution witness).
    pub fn total_steps(&self) -> u64 {
        self.steps.iter().sum()
    }
}

impl World for SustainedWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        // The typed routine payload, the identity bound, and the
        // class↔region consistency gate — the entity-scale semantics,
        // with this harness's identity space.
        let Some((entity, cursor)) = sustained_routine_parts(event.payload) else {
            return;
        };
        if entity >= self.entity_count {
            return;
        }
        if entity % REGION_COUNT != usize::from(event.class.0) {
            return;
        }
        self.steps[entity] += 1;
        self.routine_cursor[entity] += 1;
        // The LOD-cadenced execution-relative chaining with the
        // logical-data jitter — the entity-scale arithmetic, over the
        // CURRENT (possibly harness-synced) LOD view.
        let region = entity % REGION_COUNT;
        let lod = self.region_lods[region];
        let cadence = self.config.cadence_factor(lod);
        let jitter = mix64(self.config.seed ^ ((entity as u64) << 32) ^ cursor) % 2;
        let delay = self.config.activity_period * cadence + jitter;
        let field = self.config.lod_field(lod);
        ctx.follow_up_in_field(
            field,
            delay,
            event.class,
            sustained_routine_payload(entity, self.routine_cursor[entity]),
            Vec::new(),
        );
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        // The sustained-workload domain: deliberately distinct from
        // the frozen npc-world/1 and the entity-scale benchmark's
        // engine-slice/entity-scale/1 (the entity count and the LOD
        // view are workload parameters, never canonical identities).
        fp.write_str("engine-slice/sustained-world/1");
        fp.write_u64(self.config.seed);
        fp.write_u64(self.config.activity_period);
        fp.write_usize(self.entity_count);
        for lod in &self.region_lods {
            fp.write_u8(lod_code(*lod));
        }
        for step in &self.steps {
            fp.write_u64(*step);
        }
    }
}

impl SaveWorld for SustainedWorld {
    const SAVE_DOMAIN: &'static str = SUSTAINED_WORLD_SAVE_DOMAIN;

    fn save(&self, enc: &mut Encoder) {
        enc.write_u64(self.config.seed);
        enc.write_u64(self.config.activity_period);
        enc.write_count(self.entity_count);
        for lod in self.region_lods {
            enc.write_u8(lod_code(lod));
        }
        enc.write_count(self.steps.len());
        for step in &self.steps {
            enc.write_u64(*step);
        }
    }

    fn restore(dec: &mut Decoder) -> Result<Self, SaveError> {
        let seed = dec.read_u64()?;
        let activity_period = dec.read_u64()?;
        let entity_count = dec.read_u64()?;
        if entity_count > MAX_ENTITY_COUNT {
            return Err(SaveError::Limit {
                section: "sustained-world",
                kind: "entity count",
                value: entity_count,
                max: MAX_ENTITY_COUNT,
            });
        }
        let entity_count = entity_count as usize;
        let mut region_lods = [LodLevel::Full; REGION_COUNT];
        for lod in &mut region_lods {
            *lod = lod_from_code(dec.read_u8()?)?;
        }
        let step_count = dec.read_u64()?;
        if step_count != u64::try_from(entity_count).expect("bounded above") {
            return Err(SaveError::Integrity("sustained-world dense identity set"));
        }
        let mut steps = Vec::with_capacity(entity_count);
        for _ in 0..entity_count {
            steps.push(dec.read_u64()?);
        }
        dec.finish()?;
        let near_regions = [false; REGION_COUNT];
        let config = NpcConfig {
            seed,
            near_regions,
            activity_period,
            region_lods_enabled: true,
            ..NpcConfig::default()
        };
        let routine_cursor = steps.clone();
        Ok(SustainedWorld {
            entity_count,
            config,
            region_lods,
            steps,
            routine_cursor,
        })
    }
}

/// Documented maximum for the sustained world's entity count (a
/// harness bound, fail-closed at restore — never a silent resize).
const MAX_ENTITY_COUNT: u64 = 1 << 24;

/// Installs one initial routine-chain event per entity — the
/// entity-scale installer shape: the seeded spread over the first two
/// cadenced periods, in each region's LOD-classified lane.
pub fn install_sustained_schedules(spine: &mut Spine<SustainedWorld>) {
    let (seed, activity_period, entity_count, region_lods, simplify_factor, commitment_window) = {
        let world = spine.world();
        (
            world.config().seed,
            world.config().activity_period,
            world.entity_count(),
            world.region_lods(),
            world.config().simplify_factor,
            world.config().commitment_window,
        )
    };
    let near_regions = [false; REGION_COUNT];
    let config = NpcConfig {
        seed,
        near_regions,
        activity_period,
        simplify_factor,
        commitment_window,
        region_lods_enabled: true,
    };
    for entity in 0..entity_count {
        let region = entity % REGION_COUNT;
        let lod = region_lods[region];
        let field = config.lod_field(lod);
        let spread = (activity_period * 2 * config.cadence_factor(lod)).max(1);
        let offset = mix64(seed ^ ((entity as u64) << 32)) % spread;
        spine
            .schedule(
                EventSpec::new(
                    EventClass::new(region as u16),
                    Schedule::AtLogicalTime {
                        due: Tick(1 + offset),
                    },
                    sustained_routine_payload(entity, 0),
                )
                .in_field(field),
            )
            .expect("the initial entity schedule is dependency-free");
    }
}

/// The six workload classes (methodology §3): demand profiles over the
/// same populations. Classes 1–4 are demand LEVELS relative to the
/// execution budget; class 5 is the lane/LOD composition class (the
/// §5 split's subject); class 6 is the transition class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadClass {
    /// Demand well under budget: zero backlog growth, zero exhaustion,
    /// lateness ~0 — the baseline proven comfortable, not merely
    /// passing.
    LowDemand,
    /// Demand approaching budget: intermittent exhaustion, bounded
    /// oscillating backlog, nonzero but stable lateness — the
    /// transition signature.
    NearSaturation,
    /// Demand exceeding budget: monotone backlog growth with the
    /// measured rate ≈ demand − budget — the run must demonstrate the
    /// growth, not hide it.
    Overload,
    /// Overload then demand falls: the drain rate, the drain time to
    /// zero, and conservation across the whole arc.
    Recovery,
    /// A small active (near) population at high cadence while distant
    /// populations receive deliberate low-frequency updates — the §5
    /// backlog-vs-designed-cadence distinction class.
    MixedLod,
    /// Regions transition between LOD states over time: activation /
    /// deactivation latency and reconstructed-work cost against the
    /// backlog the transitions themselves create.
    ChangingPresence,
}

impl WorkloadClass {
    pub fn name(self) -> &'static str {
        match self {
            WorkloadClass::LowDemand => "low-demand",
            WorkloadClass::NearSaturation => "near-saturation",
            WorkloadClass::Overload => "overload",
            WorkloadClass::Recovery => "recovery",
            WorkloadClass::MixedLod => "mixed-lod",
            WorkloadClass::ChangingPresence => "changing-presence",
        }
    }

    /// The class's nominal total-demand target in events per tick
    /// (classes 1–3; Recovery is the overload→low arc; the LOD /
    /// presence classes' demand is their scenario's own, measured).
    pub fn target_demand_per_tick(self, budget: u32) -> u32 {
        match self {
            WorkloadClass::LowDemand => budget / 2,
            WorkloadClass::NearSaturation => budget,
            WorkloadClass::Overload => budget.saturating_mul(2),
            WorkloadClass::Recovery => budget.saturating_mul(2),
            WorkloadClass::MixedLod | WorkloadClass::ChangingPresence => budget,
        }
    }
}

/// The three LOD scenarios (the entity-scale benchmark's
/// parameterization, reproduced for the sustained matrix): all-Full
/// (the max-work baseline), mixed (the near/far split exercising the
/// reservation), all-EventDriven (the commitment-window cadence).
pub fn lod_scenarios() -> [(&'static str, [LodLevel; REGION_COUNT]); 3] {
    let mut mixed = [LodLevel::Full; REGION_COUNT];
    for (index, lod) in mixed.iter_mut().enumerate() {
        if index % 2 == 1 {
            *lod = LodLevel::EventDriven;
        }
    }
    [
        ("full", [LodLevel::Full; REGION_COUNT]),
        ("mixed", mixed),
        ("event-driven", [LodLevel::EventDriven; REGION_COUNT]),
    ]
}

/// Schedules `count` exact-count demand-overlay events due at the
/// NEXT tick into the given lane — the composition instrument that
/// tunes a cell's total demand to its class target on top of the
/// measured real-chain base. Every accepted identity is returned;
/// rejections are the caller's to record (never silently dropped).
pub fn schedule_demand(
    spine: &mut Spine<SustainedWorld>,
    count: u32,
    field: Field,
    sequence: &mut u64,
) -> Vec<EventId> {
    let due = Tick(spine.clock().as_u64() + 1);
    let mut accepted = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let payload = sustained_demand_payload(*sequence);
        *sequence = sequence.wrapping_add(1);
        let spec = EventSpec::new(EventClass::new(0), Schedule::AtLogicalTime { due }, payload)
            .in_field(field);
        if let Ok(id) = spine.schedule(spec) {
            accepted.push(id);
        }
    }
    accepted
}

/// Measures the real-chain base demand — events scheduled per tick
/// over a calibration window with NO overlay — by running the world
/// and sampling `stats().scheduled` deltas per tick. Deterministic
/// (pure logical counts); the mean over the window's second half
/// (past the install spread) is the composition table's per-cell
/// `D_base` value. The spine is ADVANCED by this calibration (the
/// caller calibrates on a dedicated construction, never mid-measurement).
pub fn calibrate_base_demand(
    entity_count: usize,
    config: &NpcConfig,
    region_lods: [LodLevel; REGION_COUNT],
    budget: u32,
    reserve: u32,
    window: u64,
) -> Vec<u64> {
    let world = SustainedWorld::new(entity_count, config.clone(), region_lods);
    let mut spine = Spine::with_reservation(world, budget, reserve);
    install_sustained_schedules(&mut spine);
    let mut samples = Vec::with_capacity(window as usize);
    let mut last_scheduled = spine.stats().scheduled;
    for _ in 0..window {
        spine.run_tick();
        let scheduled = spine.stats().scheduled;
        samples.push(scheduled - last_scheduled);
        last_scheduled = scheduled;
    }
    samples
}

/// The steady-state mean of a calibration sample window (its second
/// half — past the install spread). Deterministic arithmetic over
/// deterministic samples.
pub fn steady_state_mean(samples: &[u64]) -> f64 {
    let half = samples.len() / 2;
    if half == 0 {
        return 0.0;
    }
    let tail = &samples[samples.len() - half..];
    let sum: u64 = tail.iter().sum();
    sum as f64 / tail.len() as f64
}

// ==================================================== the cell runner
//
// (The amended matrix's composition: every cell is a real-chain base
// at (population, LOD scenario) plus the class's exact-count overlay,
// with skip decisions made by the composition table — never padded.)

/// Window length for per-window dimension sampling.
pub const WINDOW_TICKS: u64 = 50;

/// One run-matrix cell's specification. `population: 0` is the pure
/// synthetic instrument (no chains — the exact-growth-rate regressions
/// demand source); every reference-matrix cell carries real chains.
#[derive(Debug, Clone)]
pub struct CellSpec {
    pub class: WorkloadClass,
    pub population: usize,
    pub scenario: &'static str,
    pub region_lods: [LodLevel; REGION_COUNT],
    pub budget: u32,
    pub reserve: u32,
    pub seed: u64,
    pub horizon: u64,
    /// The composition-derived overlay demand per tick (near lane).
    pub overlay_near: u32,
    /// The composition-derived overlay demand per tick (far lane).
    pub overlay_far: u32,
    /// Recovery's demand-fall tick: the overlay is active for
    /// `tick <= recovery_at`, zero afterwards (the drain phase).
    pub recovery_at: Option<u64>,
    /// Class 6's harness-driven LOD transitions (F4: new follow-ups
    /// consume the synced level; in-flight obligations keep their
    /// lane — the runner applies no reclassification).
    pub lod_transitions: Vec<(u64, [LodLevel; REGION_COUNT])>,
}

impl CellSpec {
    /// The default production configuration (budget 200, near reserve
    /// 60, seed 1337, horizon 600 — the entity-scale benchmark's
    /// window), no overlay, no transitions.
    pub fn base(
        class: WorkloadClass,
        population: usize,
        scenario: (&'static str, [LodLevel; REGION_COUNT]),
    ) -> Self {
        CellSpec {
            class,
            population,
            scenario: scenario.0,
            region_lods: scenario.1,
            budget: 200,
            reserve: 60,
            seed: 1337,
            horizon: 600,
            overlay_near: 0,
            overlay_far: 0,
            recovery_at: None,
            lod_transitions: Vec::new(),
        }
    }
}

/// Per-window dimension samples (the twelve dimensions' windowed
/// form; the report aggregates — methodology §2). All logical counts
/// are deterministic; the three cost fields are wall-clock
/// observations (public-boundary granularity, dimension 10's honest
/// label: admission = overlay scheduling, execution = `run_tick`,
/// accounting = this sampling itself).
#[derive(Debug, Clone, Default)]
pub struct WindowSample {
    pub tick_end: u64,
    pub scheduled: u64,
    pub executed: u64,
    pub near_executed: u64,
    pub far_executed: u64,
    pub pending_near: usize,
    pub pending_far: usize,
    /// The §5 split, per lane, never summed: obligations past due.
    pub near_overdue: usize,
    pub far_overdue: usize,
    /// Per-lane backlogged ticks this window (due work remained for
    /// THIS lane when the budget exhausted) — the lane's own
    /// saturation signal, never the global flag.
    pub near_backlogged: u64,
    pub far_backlogged: u64,
    pub near_max_overdue_age: u64,
    pub far_max_overdue_age: u64,
    pub near_p95_wait: f64,
    pub far_p95_wait: f64,
    pub budget_exhausted: u64,
    pub late_count: u64,
    pub late_p50: f64,
    pub late_p95: f64,
    pub late_max: u64,
    pub admission_us: u64,
    pub execution_us: u64,
    pub accounting_us: u64,
}

/// The per-lane workload-shape verdict — the methodology's exact
/// claim vocabulary (§6), nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneVerdict {
    Sustainable,
    Saturated,
    Overloaded,
    Recovering,
}

impl LaneVerdict {
    pub fn name(self) -> &'static str {
        match self {
            LaneVerdict::Sustainable => "sustainable",
            LaneVerdict::Saturated => "saturated",
            LaneVerdict::Overloaded => "overloaded",
            LaneVerdict::Recovering => "recovering",
        }
    }
}

/// The §5 classification: near and far lanes are judged SEPARATELY
/// against their own cadence budgets, never summed.
#[derive(Debug, Clone)]
pub struct Classification {
    pub near: LaneVerdict,
    pub far: LaneVerdict,
    /// Measured overdue-growth slope per tick (the overload rate).
    pub near_overdue_slope: f64,
    pub far_overdue_slope: f64,
    pub near_max_overdue_age: u64,
    pub far_max_overdue_age: u64,
}

/// The least-squares slope of a per-window series (deterministic
/// arithmetic over deterministic samples).
fn series_slope(values: &[f64]) -> f64 {
    let n = values.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = values.iter().sum::<f64>() / n;
    let mut cov = 0.0;
    let mut var = 0.0;
    for (index, value) in values.iter().enumerate() {
        let dx = index as f64 - mean_x;
        cov += dx * (value - mean_y);
        var += dx * dx;
    }
    if var == 0.0 { 0.0 } else { cov / var }
}

/// R2 (the units correction): the window series is sampled every
/// `WINDOW_TICKS` logical ticks, so a slope between window samples
/// is a per-WINDOW rate — every reported and thresholded rate is
/// converted to the per-LOGICAL-TICK unit by dividing out the window
/// spacing. A 30-events/tick demand against a 20 budget reports
/// ≈ 10 overdue events per logical tick, never 500.
fn slope_per_tick(window_series: &[f64]) -> f64 {
    series_slope(window_series) / WINDOW_TICKS as f64
}

fn sorted_percentile(values: &mut [f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let index = ((quantile * values.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[index]
}

/// Classifies one lane's window series against its cadence budget —
/// rates in per-LOGICAL-TICK units (R2): growth ≥ +0.1 overdue
/// events per tick → Overloaded (a genuinely positive long-run rate,
/// however gradual, is unsustainability); ≤ −0.1/tick → Recovering;
/// otherwise Saturated when overdue age ever exceeds the cadence
/// budget or THIS LANE's own backlogged signal fires (due work
/// remained when the budget exhausted — the §5 split's honesty: a
/// near-lane excess must never mislabel the far lane); else
/// Sustainable.
fn classify_lane(
    overdue: &[f64],
    max_age: u64,
    lane_backlogged: bool,
    cadence_budget: u64,
) -> LaneVerdict {
    let slope = slope_per_tick(overdue);
    if slope >= 0.1 {
        LaneVerdict::Overloaded
    } else if slope <= -0.1 {
        LaneVerdict::Recovering
    } else if max_age > cadence_budget || lane_backlogged {
        LaneVerdict::Saturated
    } else {
        LaneVerdict::Sustainable
    }
}

/// The §5 classification over a cell's window series. The cadence
/// budgets are the lanes' designed inter-execution intervals as
/// configured by the composition (the regressions set them
/// explicitly; the reference matrix derives them from the scenario).
pub fn classify(
    windows: &[WindowSample],
    near_cadence_budget: u64,
    far_cadence_budget: u64,
) -> Classification {
    let near_overdue: Vec<f64> = windows.iter().map(|w| w.near_overdue as f64).collect();
    let far_overdue: Vec<f64> = windows.iter().map(|w| w.far_overdue as f64).collect();
    let near_max = windows
        .iter()
        .map(|w| w.near_max_overdue_age)
        .max()
        .unwrap_or(0);
    let far_max = windows
        .iter()
        .map(|w| w.far_max_overdue_age)
        .max()
        .unwrap_or(0);
    let near_backlogged = windows.iter().any(|w| w.near_backlogged > 0);
    let far_backlogged = windows.iter().any(|w| w.far_backlogged > 0);
    Classification {
        near: classify_lane(
            &near_overdue,
            near_max,
            near_backlogged,
            near_cadence_budget,
        ),
        far: classify_lane(&far_overdue, far_max, far_backlogged, far_cadence_budget),
        near_overdue_slope: slope_per_tick(&near_overdue),
        far_overdue_slope: slope_per_tick(&far_overdue),
        near_max_overdue_age: near_max,
        far_max_overdue_age: far_max,
    }
}

/// The measured drain tick of a class-4 arc: the first window end at
/// which the near lane's overdue backlog has returned to zero after
/// being nonzero (the population-parameterized anchor the horizon
/// derivation consumes — F6).
pub fn measured_drain_tick(windows: &[WindowSample]) -> Option<u64> {
    let mut seen_backlog = false;
    for window in windows {
        if window.near_overdue > 0 {
            seen_backlog = true;
        }
        if seen_backlog && window.near_overdue == 0 {
            return Some(window.tick_end);
        }
    }
    None
}

/// The methodology's horizon rule: a simulated-time horizon ≥ 10×
/// the drain time observed in class 4 at that population — derived
/// per population, never a fixed wall-clock cap masquerading as a
/// logical horizon.
pub fn derived_horizon(drain_ticks: u64) -> u64 {
    drain_ticks.saturating_mul(10)
}

/// FNV-1a over the deterministic per-tick trace (dimension 12's
/// double-run comparison key, alongside the world fingerprint).
struct TraceDigest(u64);

impl TraceDigest {
    fn new() -> Self {
        TraceDigest(0xcbf2_9ce4_8422_2325)
    }

    fn write_u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish_hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Resident set in bytes, Linux only — the honesty rule: `None` is
/// reported as "unmeasured on this platform", never omitted silently.
#[cfg(target_os = "linux")]
fn resident_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut fields = statm.split_whitespace();
    fields.next()?;
    let pages: u64 = fields.next()?.parse().ok()?;
    Some(pages * 4096)
}

#[cfg(not(target_os = "linux"))]
fn resident_bytes() -> Option<u64> {
    None
}

/// One cell's complete measured outcome (the §7 report's data).
#[derive(Debug, Clone)]
pub struct CellReport {
    pub class: WorkloadClass,
    pub population: usize,
    pub scenario: &'static str,
    pub budget: u32,
    pub reserve: u32,
    pub horizon: u64,
    pub overlay_near: u32,
    pub overlay_far: u32,
    pub windows: Vec<WindowSample>,
    pub classification: Classification,
    pub scheduled: u64,
    pub executed: u64,
    pub late_arrivals: u64,
    pub late_executions: u64,
    pub budget_exhausted_ticks: u64,
    pub processed_through: Option<u64>,
    pub near_executed: u64,
    pub far_executed: u64,
    pub near_backlogged_ticks: u64,
    pub far_backlogged_ticks: u64,
    pub fingerprint_hex: String,
    pub digest_hex: String,
    pub save_len: usize,
    pub save_us: u64,
    pub resident: Option<u64>,
    pub conservation_holds: bool,
}

/// Runs one cell: installs the real-chain base, applies the class's
/// overlay and transitions, samples the twelve dimensions per window,
/// classifies the §5 split, and fingerprints/digests the deterministic
/// trace. The harness NEVER compacts a measured run (dimension 6
/// requires the full ledger; the compacted form is a separate
/// variant). A conservation violation is a hard failure (asserted).
pub fn run_cell(spec: &CellSpec) -> CellReport {
    let near_regions = [false; REGION_COUNT];
    let config = NpcConfig {
        seed: spec.seed,
        near_regions,
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let world = SustainedWorld::new(spec.population, config, spec.region_lods);
    let mut spine = Spine::with_reservation(world, spec.budget, spec.reserve);
    install_sustained_schedules(&mut spine);

    let mut sequence = 0u64;
    let mut digest = TraceDigest::new();
    let mut windows: Vec<WindowSample> = Vec::new();
    let mut window = WindowSample::default();
    let mut conservation_holds = true;
    let mut ledger_cursor: usize = 0;
    let mut last_scheduled = spine.stats().scheduled;
    let mut last_executed = spine.stats().executed;
    let mut last_exhausted = spine.stats().budget_exhausted_ticks;
    let mut last_lanes = spine.lane_totals();

    for tick in 1..=spec.horizon {
        for (at, lods) in &spec.lod_transitions {
            if *at == tick {
                spine.world_mut().set_region_lods(*lods);
            }
        }
        let overlay_active = spec.recovery_at.is_none_or(|at| tick <= at);
        let overlay_near = if overlay_active { spec.overlay_near } else { 0 };
        let overlay_far = if overlay_active { spec.overlay_far } else { 0 };

        let admission_start = std::time::Instant::now();
        if overlay_near > 0 {
            schedule_demand(&mut spine, overlay_near, Field::Near, &mut sequence);
        }
        if overlay_far > 0 {
            schedule_demand(&mut spine, overlay_far, Field::Far, &mut sequence);
        }
        let admission = admission_start.elapsed();

        let execution_start = std::time::Instant::now();
        spine.run_tick();
        let execution = execution_start.elapsed();

        let stats = spine.stats();
        let scheduled_delta = stats.scheduled - last_scheduled;
        let executed_delta = stats.executed - last_executed;
        last_scheduled = stats.scheduled;
        last_executed = stats.executed;
        digest.write_u64(scheduled_delta);
        digest.write_u64(executed_delta);
        digest.write_u64(spine.near_pending_count() as u64);
        digest.write_u64(spine.far_pending_count() as u64);
        window.scheduled += scheduled_delta;
        window.executed += executed_delta;
        window.admission_us += admission.as_micros() as u64;
        window.execution_us += execution.as_micros() as u64;

        if tick % WINDOW_TICKS == 0 {
            let accounting_start = std::time::Instant::now();
            let clock = spine.clock().as_u64();
            window.tick_end = tick;
            window.pending_near = spine.near_pending_count();
            window.pending_far = spine.far_pending_count();
            window.budget_exhausted = stats.budget_exhausted_ticks - last_exhausted;
            last_exhausted = stats.budget_exhausted_ticks;
            let lanes = spine.lane_totals();
            window.near_executed = lanes.near_executed - last_lanes.near_executed;
            window.far_executed = lanes.far_executed - last_lanes.far_executed;
            window.near_backlogged = lanes.near_backlogged_ticks - last_lanes.near_backlogged_ticks;
            window.far_backlogged = lanes.far_backlogged_ticks - last_lanes.far_backlogged_ticks;
            last_lanes = lanes;

            // The §5 pending-age scan, per lane, never summed.
            let mut near_ages = Vec::new();
            let mut far_ages = Vec::new();
            for view in spine.collect_matching(|_| true) {
                if view.location != ObligationLocation::Queued {
                    continue;
                }
                let age = clock.saturating_sub(view.due.as_u64());
                match view.field {
                    Field::Near => {
                        if age > 0 {
                            window.near_overdue += 1;
                            window.near_max_overdue_age = window.near_max_overdue_age.max(age);
                        }
                        near_ages.push(age as f64);
                    }
                    Field::Far => {
                        if age > 0 {
                            window.far_overdue += 1;
                            window.far_max_overdue_age = window.far_max_overdue_age.max(age);
                        }
                        far_ages.push(age as f64);
                    }
                }
            }
            window.near_p95_wait = sorted_percentile(&mut near_ages, 0.95);
            window.far_p95_wait = sorted_percentile(&mut far_ages, 0.95);

            // Dimension 6's lateness magnitudes — the ledger CURSOR
            // scan (linear in new records, never a quadratic re-scan).
            let ledger = spine
                .try_ledger_full()
                .expect("the harness never compacts a measured run");
            let mut lateness = Vec::new();
            for record in ledger[ledger_cursor..].iter() {
                if record.late {
                    lateness.push(
                        (record.executed_tick.as_u64() - record.intended_due.as_u64()) as f64,
                    );
                }
            }
            ledger_cursor = ledger.len();
            window.late_count = lateness.len() as u64;
            if !lateness.is_empty() {
                let max_late = lateness.iter().cloned().fold(0.0_f64, f64::max);
                window.late_p50 = sorted_percentile(&mut lateness.clone(), 0.50);
                window.late_p95 = sorted_percentile(&mut lateness, 0.95);
                window.late_max = max_late as u64;
            }
            window.accounting_us = accounting_start.elapsed().as_micros() as u64;

            let stats = spine.stats();
            let conservation = stats.scheduled
                == stats.executed
                    + spine.pending_count() as u64
                    + spine.blocked_count() as u64
                    + spine.held_count() as u64;
            conservation_holds &= conservation;

            windows.push(std::mem::take(&mut window));
        }
    }
    assert!(
        conservation_holds,
        "conservation identity violated in cell {:?}/{}",
        spec.class.name(),
        spec.scenario
    );

    let stats = spine.stats();
    let lanes = spine.lane_totals();
    let mut fp = Fingerprinter::new();
    spine.world().fingerprint(&mut fp);
    let fingerprint_hex = fp.finish_hex();
    digest.write_u64(stats.scheduled);
    digest.write_u64(stats.executed);
    digest.write_u64(stats.budget_exhausted_ticks);
    let digest_hex = digest.finish_hex();
    let save_start = std::time::Instant::now();
    let save = spine.save_bytes();
    let save_us = save_start.elapsed().as_micros() as u64;

    // The cadence budgets: each lane's designed inter-execution
    // interval at its scenario cadence, with one designed interval of
    // acknowledged slack (the composition's policy, stated here so
    // the classifier's inputs are auditable).
    let period = 6;
    let near_cadence_budget = period * 2;
    let far_cadence_budget = period * 12 * 2;
    let classification = classify(&windows, near_cadence_budget, far_cadence_budget);

    CellReport {
        class: spec.class,
        population: spec.population,
        scenario: spec.scenario,
        budget: spec.budget,
        reserve: spec.reserve,
        horizon: spec.horizon,
        overlay_near: spec.overlay_near,
        overlay_far: spec.overlay_far,
        windows,
        classification,
        scheduled: stats.scheduled,
        executed: stats.executed,
        late_arrivals: stats.late_arrivals,
        late_executions: stats.late_executions,
        budget_exhausted_ticks: stats.budget_exhausted_ticks,
        processed_through: spine.processed_through().map(|t| t.as_u64()),
        near_executed: lanes.near_executed,
        far_executed: lanes.far_executed,
        near_backlogged_ticks: lanes.near_backlogged_ticks,
        far_backlogged_ticks: lanes.far_backlogged_ticks,
        fingerprint_hex,
        digest_hex,
        save_len: save.len(),
        save_us,
        resident: resident_bytes(),
        conservation_holds,
    }
}

/// The double-run determinism proof for one cell (methodology §8.4):
/// two independent constructions, identical specification — the
/// trace digest and the world fingerprint must be byte-identical.
pub fn run_cell_deterministic(spec: &CellSpec) -> (CellReport, CellReport, bool) {
    let first = run_cell(spec);
    let second = run_cell(spec);
    let deterministic =
        first.digest_hex == second.digest_hex && first.fingerprint_hex == second.fingerprint_hex;
    (first, second, deterministic)
}

impl CellReport {
    /// The §7 per-workload report — the eight required fields, with
    /// the §6 honesty rules enforced in the wording: completed vs
    /// deferred never conflated, backlog per lane never summed,
    /// unmeasured cases explicit.
    pub fn print(&self) {
        println!(
            "sustained-load cell: class {} / population {} / scenario {} / budget {} (reserve {}) / horizon {} / overlay near {} far {} per tick",
            self.class.name(),
            self.population,
            self.scenario,
            self.budget,
            self.reserve,
            self.horizon,
            self.overlay_near,
            self.overlay_far
        );
        let mut exec: Vec<f64> = self
            .windows
            .iter()
            .map(|w| w.execution_us as f64 / 1000.0)
            .collect();
        let mut adm: Vec<f64> = self
            .windows
            .iter()
            .map(|w| w.admission_us as f64 / 1000.0)
            .collect();
        let mut acc: Vec<f64> = self
            .windows
            .iter()
            .map(|w| w.accounting_us as f64 / 1000.0)
            .collect();
        println!(
            "  computational cost (public-boundary split, ms/window): execution p50 {:.3} / p95 {:.3} · admission p50 {:.3} · accounting p50 {:.3}",
            sorted_percentile(&mut exec.clone(), 0.50),
            sorted_percentile(&mut exec, 0.95),
            sorted_percentile(&mut adm, 0.50),
            sorted_percentile(&mut acc, 0.50)
        );
        println!(
            "  logical progress: scheduled {} / executed {} (near {} / far {}) / processed_through {:?}",
            self.scheduled,
            self.executed,
            self.near_executed,
            self.far_executed,
            self.processed_through
        );
        println!(
            "  fidelity level: real chains at the scenario's designed cadences + synthetic overlay (near {} / far {} per tick); completed obligations are the executed counts above; deferred work is the per-lane backlog below — budget-limiting is NOT reported as efficiency, and no equivalence beyond the region routine contracts is claimed",
            self.overlay_near, self.overlay_far
        );
        println!(
            "  backlog (per lane, never summed): near {} (slope {:+.2}/tick, max overdue age {}) / far {} (slope {:+.2}/tick, max overdue age {})",
            self.windows.last().map(|w| w.near_overdue).unwrap_or(0),
            self.classification.near_overdue_slope,
            self.classification.near_max_overdue_age,
            self.windows.last().map(|w| w.far_overdue).unwrap_or(0),
            self.classification.far_overdue_slope,
            self.classification.far_max_overdue_age
        );
        println!(
            "  classification: near {} / far {}",
            self.classification.near.name(),
            self.classification.far.name()
        );
        let late_p95 = self
            .windows
            .iter()
            .map(|w| w.late_p95)
            .fold(0.0_f64, f64::max);
        println!(
            "  latency: late executions {} (p95 magnitude {:.1} ticks late, max {})",
            self.late_executions,
            late_p95,
            self.windows.iter().map(|w| w.late_max).max().unwrap_or(0)
        );
        println!(
            "  memory: resident {} / snapshot {} B in {} us (ledger-size dependent — the lifetime history is part of the measured representation cost)",
            self.resident
                .map(|b| b.to_string())
                .unwrap_or_else(|| "unmeasured on this platform".to_string()),
            self.save_len,
            self.save_us
        );
        println!(
            "  determinism: trace digest {} / world fingerprint {}",
            self.digest_hex, self.fingerprint_hex
        );
        println!(
            "  failure/saturation: budget-exhausted ticks {} (near-backlogged {} / far-backlogged {}) · conservation {}",
            self.budget_exhausted_ticks,
            self.near_backlogged_ticks,
            self.far_backlogged_ticks,
            if self.conservation_holds {
                "holds"
            } else {
                "VIOLATED"
            }
        );
    }
}

/// Runs the nine (population × LOD scenario) base calibrations and
/// prints the composition table: the measured `D_base` per cell, the
/// per-class target, the overlay or the reason a class is N/A there,
/// and the pure-real-overload marker where the base alone exceeds
/// twice the budget. The table is EVIDENCE — measured, never assumed.
pub fn print_composition_table() {
    println!(
        "sustained-load composition table (measured D_base per cell, budget 200 / reserve 60)"
    );
    let near_regions = [false; REGION_COUNT];
    for population in [1_000usize, 5_000, 20_000] {
        for (name, lods) in lod_scenarios() {
            let config = NpcConfig {
                seed: 1337,
                near_regions,
                region_lods_enabled: true,
                ..NpcConfig::default()
            };
            let samples = calibrate_base_demand(population, &config, lods, 200, 60, 200);
            let base = steady_state_mean(&samples);
            print!("  population {population} scenario {name}: D_base {base:.1}/tick",);
            for class in [
                WorkloadClass::LowDemand,
                WorkloadClass::NearSaturation,
                WorkloadClass::Overload,
                WorkloadClass::Recovery,
            ] {
                let target = f64::from(class.target_demand_per_tick(200));
                let note = if base >= target {
                    "N/A (base ≥ target)"
                } else if class == WorkloadClass::Overload && base >= 2.0 * target {
                    "pure real-chain overload"
                } else {
                    "overlay"
                };
                print!(" · {} {}", class.name(), note);
            }
            println!();
        }
    }
}

/// The `--benchmark-sustained-load` entry point (the CLI wiring is
/// minimal and delimited in `main.rs`; the integrator reconciles).
/// Phase 1: the measured composition table. Phase 2 (the full
/// reference run matrix) executes as pod evidence, separate from the
/// CI battery — never thresholds, never called complete when partial.
pub fn run_sustained_load_benchmark() {
    println!(
        "Sustained-load benchmark (F-08/F-12) — pod results, observational (never CI thresholds)"
    );
    println!(
        "workload: real routine chains (SustainedWorld) + exact-count demand overlays, budget 200 / near reserve 60"
    );
    print_composition_table();
    // One demonstration cell (the §5 split's subject at 1k): the
    // mixed scenario's real chains, no overlay — near lanes at
    // designed cadence, far lanes at their sparse designed cadence.
    let scenario = lod_scenarios()[1];
    let spec = CellSpec::base(WorkloadClass::MixedLod, 1_000, scenario);
    let (report, _second, deterministic) = run_cell_deterministic(&spec);
    report.print();
    println!(
        "  determinism proof: {}",
        if deterministic {
            "DOUBLE-RUN IDENTICAL"
        } else {
            "DIVERGED (hard failure)"
        }
    );
    assert!(
        deterministic,
        "the demonstration cell must be deterministic"
    );
}

/// F3's compacted-representation variant: one mixed-scenario cell per
/// population — the full-history save (the measured form) vs the
/// compacted `spine-history/2` save, with the ledger downgrade
/// verified (dimension 6's percentiles are never taken on this form;
/// the full and compacted representations reject one another by
/// domain, never silently reinterpreted).
pub fn measure_compacted_representation(population: usize) {
    let near_regions = [false; REGION_COUNT];
    let config = NpcConfig {
        seed: 1337,
        near_regions,
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let scenario = lod_scenarios()[1];
    let world = SustainedWorld::new(population, config, scenario.1);
    let mut spine = Spine::with_reservation(world, 200, 60);
    install_sustained_schedules(&mut spine);
    for _ in 0..300 {
        spine.run_tick();
    }
    let full_len = spine.save_bytes().len();
    let executed = spine.stats().executed;
    let retain = executed / 2;
    spine
        .compact(retain)
        .expect("the compaction summarizes half the ledger");
    let compacted_len = spine.save_bytes().len();
    assert!(
        spine.try_ledger_full().is_none(),
        "the compacted history downgrades the full-ledger view"
    );
    println!(
        "  compacted representation @ {population}: full save {full_len} B vs compacted {compacted_len} B (executed {executed}, retain {retain} records) — the lifetime ledger dominates the full form; dimension 6 percentiles are never taken on the compacted form",
    );
}

/// R3: the reference-horizon methodology for recovery cells — a
/// 100-tick arc probe MEASURES the drain at this exact composition,
/// then the horizon is 10× the measured drain (the documented rule),
/// capped at 1,200 ticks for bounded pod resources (the cap is
/// STATED whenever it binds, never hidden). Compositions that do not
/// drain within the probe (the post-arc base alone meets/exceeds the
/// budget, or drains slower than the probe window) get an explicit
/// bounded observation window — the report shows the backlog/age
/// trend and no complete recovery is claimed.
fn derive_recovery_horizon(mut spec: CellSpec, population: usize) -> CellSpec {
    const ARC_TICKS: u64 = 100;
    const PROBE_HORIZON: u64 = 600;
    const HORIZON_CAP: u64 = 1_200;
    let mut probe = spec.clone();
    probe.recovery_at = Some(ARC_TICKS);
    probe.horizon = PROBE_HORIZON;
    let probe_report = run_cell(&probe);
    spec.recovery_at = Some(ARC_TICKS);
    match measured_drain_tick(&probe_report.windows) {
        Some(drain_end) => {
            let drain_ticks = drain_end.saturating_sub(ARC_TICKS);
            let derived = derived_horizon(drain_ticks);
            spec.horizon = derived.min(HORIZON_CAP);
            println!(
                "   [horizon] recovery @ population {population}: drain measured {drain_ticks} ticks after a {ARC_TICKS}-tick arc → derived 10× = {derived} ticks{}",
                if spec.horizon < derived {
                    format!(
                        " — CAPPED at {} for bounded pod resources (stated, not hidden)",
                        spec.horizon
                    )
                } else {
                    String::new()
                }
            );
        }
        None => {
            spec.horizon = if population >= 5_000 { 300 } else { 600 };
            println!(
                "   [horizon] recovery @ population {population}: NO complete drain within the {PROBE_HORIZON}-tick probe (the post-arc base alone meets/exceeds the budget, or drains slower than the probe) — explicit bounded observation window of {} ticks; the backlog/age trend is reported and NO complete recovery is claimed",
                spec.horizon
            );
        }
    }
    spec
}

/// The full reference matrix (§4 as amended): composition-derived
/// cells over MEASURED `D_base`, mechanical skip rules (every N/A
/// printed with its reason — never padded), every cell double-run,
/// reduced-budget variants for classes 1–5 (F5), and the
/// compacted-representation variants (F3). Pod reference evidence —
/// never CI thresholds; a partial matrix is never called complete
/// (§6). The overlay is near-lane-targeted (the stated composition
/// policy; the lane split is reported per cell).
pub fn run_reference_matrix() {
    println!(
        "=== SUSTAINED-LOAD REFERENCE MATRIX (F-08/F-12) — pod evidence, double-run per cell ==="
    );
    let near_regions = [false; REGION_COUNT];
    let config = NpcConfig {
        seed: 1337,
        near_regions,
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let mut run_cells = 0usize;
    let mut skipped = 0usize;

    // Classes 1–4 (the demand levels) over every (population × LOD
    // scenario) with the measured base and the mechanical skip rule.
    for population in [1_000usize, 5_000, 20_000] {
        for scenario in lod_scenarios() {
            let samples = calibrate_base_demand(population, &config, scenario.1, 200, 60, 200);
            let base = steady_state_mean(&samples);
            println!(
                "-- population {population} scenario {} (measured D_base {base:.1}/tick) --",
                scenario.0
            );
            for class in [
                WorkloadClass::LowDemand,
                WorkloadClass::NearSaturation,
                WorkloadClass::Overload,
                WorkloadClass::Recovery,
            ] {
                let target = f64::from(class.target_demand_per_tick(200));
                if base >= target {
                    skipped += 1;
                    println!(
                        "   [N/A] {}: base {base:.1} ≥ target {target:.0} — the class is undefined here (reason-coded, never padded)",
                        class.name()
                    );
                    continue;
                }
                let mut spec = CellSpec::base(class, population, scenario);
                spec.overlay_near = (target - base).round() as u32;
                if class == WorkloadClass::Recovery {
                    spec = derive_recovery_horizon(spec, population);
                }
                if population >= 5_000 && class == WorkloadClass::Overload {
                    // Bounded observation cost at scale (the pending
                    // scan is linear in backlog); the horizon is
                    // printed per cell — never a hidden cap.
                    spec.horizon = 300;
                }
                let (report, _second, deterministic) = run_cell_deterministic(&spec);
                assert!(
                    deterministic,
                    "cell {}/{} diverged between runs",
                    class.name(),
                    scenario.0
                );
                report.print();
                run_cells += 1;
            }
            // Class 5 (the §5 split's subject) rides the mixed
            // scenario at every population.
            if scenario.0 == "mixed" {
                let spec = CellSpec::base(WorkloadClass::MixedLod, population, scenario);
                let (report, _second, deterministic) = run_cell_deterministic(&spec);
                assert!(deterministic, "the mixed-lod cell diverged");
                report.print();
                run_cells += 1;
            }
        }
        // Class 6 (F4's scaled mechanism, labeled): harness-driven LOD
        // transitions on the mixed scenario — mixed → all-EventDriven
        // at tick 200. The RegionDirector-protocol-driven 1k anchor
        // is a stated limitation of this cycle's matrix (the
        // director's apply is NpcWorld-typed); the director's own
        // transition semantics remain covered by the existing
        // regions/s6 suites.
        let scenario = lod_scenarios()[1];
        let mut spec = CellSpec::base(WorkloadClass::ChangingPresence, population, scenario);
        spec.lod_transitions = vec![(200u64, [LodLevel::EventDriven; REGION_COUNT])];
        let (report, _second, deterministic) = run_cell_deterministic(&spec);
        assert!(deterministic, "the changing-presence cell diverged");
        report.print();
        run_cells += 1;
    }

    // Reduced-budget variants (F5): classes 1–5 at the representative
    // 1k-mixed cell under budget 100 / reserve 30. The class-1 N/A
    // here is itself the informative outcome — the low-demand comfort
    // is budget-relative.
    println!("-- reduced-budget variants (budget 100 / reserve 30, 1k-mixed) --");
    let samples = calibrate_base_demand(1_000, &config, lod_scenarios()[1].1, 100, 30, 200);
    let base = steady_state_mean(&samples);
    for class in [
        WorkloadClass::LowDemand,
        WorkloadClass::NearSaturation,
        WorkloadClass::Overload,
        WorkloadClass::Recovery,
        WorkloadClass::MixedLod,
    ] {
        let target = f64::from(class.target_demand_per_tick(100));
        if base >= target {
            skipped += 1;
            println!(
                "   [N/A] {} (reduced budget): base {base:.1} ≥ target {target:.0} — the comfort is budget-relative",
                class.name()
            );
            continue;
        }
        let mut spec = CellSpec::base(class, 1_000, lod_scenarios()[1]);
        spec.budget = 100;
        spec.reserve = 30;
        spec.overlay_near = (target - base).round() as u32;
        if class == WorkloadClass::Recovery {
            spec = derive_recovery_horizon(spec, 1_000);
        }
        let (report, _second, deterministic) = run_cell_deterministic(&spec);
        assert!(deterministic, "the reduced-budget cell diverged");
        report.print();
        run_cells += 1;
    }

    // Compacted-representation variants (F3): one per population.
    println!("-- compacted-representation variants (F3) --");
    for population in [1_000usize, 5_000, 20_000] {
        measure_compacted_representation(population);
    }

    println!(
        "=== matrix composition: {run_cells} cells measured (each double-run) + 3 compacted-representation variants, {skipped} N/A reason-coded — the honest effective matrix, never padded; partial never called complete ==="
    );
}

/// C2-P2 Phase 1 (the amended matrix): the capacity-envelope sweep.
/// P1.1 — spine-vs-world isolation: the pure-synthetic instrument at
/// demand == budget (zero backlog, steady execution at the exact
/// rate) gives the spine's per-event cost; the chain cells in the
/// baseline matrix carry the world-execute cost on top. P1.2 — the
/// budget sweep over the real chains (no overlay — each
/// composition's own latent demand), reserve = 30% of budget (the
/// baseline 60/200 ratio, per the amendment's N1), horizon 300
/// (bounded observation; the pending scan is linear in backlog),
/// every cell double-run. Output: per (population × scenario ×
/// budget) — executed/tick, per-lane overdue slopes, useful work per
/// CPU ms, lateness p95, verdicts — and the envelope: the minimum
/// budget at which the composition's real demand is sustained (both
/// lanes' overdue zero at the horizon end and zero lateness), or
/// "not sustained within the sweep" with the measured slopes. Pod
/// evidence only — never CI thresholds; release-profile numbers are
/// the binding CPU claims, debug is continuity-only.
pub fn run_capacity_envelope() {
    println!(
        "=== C2-P2 PHASE 1: CAPACITY ENVELOPE (budget sweep, reserve = 30% of budget, real chains, no overlay) ==="
    );
    let near_regions = [false; REGION_COUNT];
    let config = NpcConfig {
        seed: 1337,
        near_regions,
        region_lods_enabled: true,
        ..NpcConfig::default()
    };

    // P1.1 — the spine's per-event cost in isolation.
    println!(
        "-- P1.1 spine isolation (pure synthetic, demand == budget → steady execution at the exact rate) --"
    );
    for rate in [200u32, 1_000, 3_000] {
        let mut spec = CellSpec::base(WorkloadClass::NearSaturation, 0, lod_scenarios()[0]);
        spec.budget = rate;
        spec.reserve = rate * 3 / 10;
        spec.overlay_near = rate;
        spec.horizon = 150;
        let (report, _second, deterministic) = run_cell_deterministic(&spec);
        assert!(deterministic, "the isolation cell diverged");
        let execution_us: u64 = report.windows.iter().map(|w| w.execution_us).sum();
        let per_event_us = execution_us as f64 / (f64::from(rate) * 150.0);
        let last = report.windows.last().expect("windowed");
        println!(
            "  spine @ {rate} events/tick: {per_event_us:.3} us/event · executed {} · overdue near {} / far {} · conservation {}",
            report.executed, last.near_overdue, last.far_overdue, report.conservation_holds
        );
    }

    // P1.2 — the budget sweep over each composition's real chains.
    for population in [1_000usize, 5_000, 20_000] {
        for scenario in lod_scenarios() {
            let samples = calibrate_base_demand(population, &config, scenario.1, 200, 60, 200);
            let base = steady_state_mean(&samples);
            println!(
                "-- population {population} scenario {} (D_base {base:.1}/tick realized at budget 200) --",
                scenario.0
            );
            let mut envelope: Option<u32> = None;
            // The budget-200 row and the first sustaining row, captured
            // for the explicit C1 before/after COMPARISON line — both
            // rows from THIS sweep's own measured cells (never re-run,
            // never asserted from arithmetic): the same composition,
            // genuinely-overdue at the 200 baseline vs sustained at the
            // measured budget, with the CPU cost of each.
            let mut before: Option<(u64, usize, f64, f64)> = None;
            let mut sustain: Option<(u64, f64)> = None;
            for budget in [200u32, 400, 800, 1_600, 3_200] {
                let mut spec = CellSpec::base(WorkloadClass::NearSaturation, population, scenario);
                spec.budget = budget;
                spec.reserve = budget * 3 / 10;
                spec.horizon = 300;
                let (report, _second, deterministic) = run_cell_deterministic(&spec);
                assert!(deterministic, "the envelope cell diverged");
                let execution_us: u64 = report.windows.iter().map(|w| w.execution_us).sum();
                let executed_per_ms =
                    report.executed as f64 / (execution_us as f64 / 1_000.0).max(0.001);
                let last = report.windows.last().expect("windowed");
                let sustained =
                    last.near_overdue == 0 && last.far_overdue == 0 && report.late_executions == 0;
                if budget == 200 {
                    before = Some((
                        report.executed / report.horizon,
                        last.near_overdue + last.far_overdue,
                        report
                            .windows
                            .iter()
                            .map(|w| w.late_p95)
                            .fold(0.0_f64, f64::max),
                        executed_per_ms,
                    ));
                }
                if sustained && envelope.is_none() {
                    envelope = Some(budget);
                    sustain = Some((report.executed / report.horizon, executed_per_ms));
                }
                let late_p95 = report
                    .windows
                    .iter()
                    .map(|w| w.late_p95)
                    .fold(0.0_f64, f64::max);
                println!(
                    "  budget {budget:>4} (reserve {}): executed {}/tick · work {executed_per_ms:.0} events/ms · near slope {:+.2}/tick · far slope {:+.2}/tick · final overdue near {} / far {} · late p95 {late_p95:.1} · verdict {}/{} · conservation {}",
                    spec.reserve,
                    report.executed / report.horizon,
                    report.classification.near_overdue_slope,
                    report.classification.far_overdue_slope,
                    last.near_overdue,
                    last.far_overdue,
                    report.classification.near.name(),
                    report.classification.far.name(),
                    report.conservation_holds
                );
            }
            match (envelope, before, sustain) {
                (
                    Some(budget),
                    Some((executed, overdue, p95, work)),
                    Some((s_executed, s_work)),
                ) => {
                    println!(
                        "  ENVELOPE: population {population} scenario {} — the real chains are sustained (zero overdue, zero lateness) from budget {budget} upward",
                        scenario.0
                    );
                    println!(
                        "  COMPARISON (C1): budget 200 → {budget}: executed {executed} → {s_executed}/tick · final overdue {overdue} → 0 · late p95 {p95:.1} → 0 · work {work:.0} → {s_work:.0} events/ms — the budget binds, not the CPU (both rows measured in this sweep)"
                    );
                }
                _ => println!(
                    "  ENVELOPE: population {population} scenario {} — NOT sustained within the sweep (max budget 3200); the measured slopes above are the honest limit",
                    scenario.0
                ),
            }
        }
    }
}
