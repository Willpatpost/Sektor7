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

/// Classifies one lane's window series against its cadence budget:
/// growth slope ≥ +0.5/tick → Overloaded; ≤ −0.5/tick → Recovering;
/// otherwise Saturated when overdue age ever exceeds the cadence
/// budget or exhaustion occurs; else Sustainable.
fn classify_lane(
    overdue: &[f64],
    max_age: u64,
    exhausted: bool,
    cadence_budget: u64,
) -> LaneVerdict {
    let slope = series_slope(overdue);
    if slope >= 0.5 {
        LaneVerdict::Overloaded
    } else if slope <= -0.5 {
        LaneVerdict::Recovering
    } else if max_age > cadence_budget || exhausted {
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
    let exhausted = windows.iter().any(|w| w.budget_exhausted > 0);
    Classification {
        near: classify_lane(&near_overdue, near_max, exhausted, near_cadence_budget),
        far: classify_lane(&far_overdue, far_max, exhausted, far_cadence_budget),
        near_overdue_slope: series_slope(&near_overdue),
        far_overdue_slope: series_slope(&far_overdue),
        near_max_overdue_age: near_max,
        far_max_overdue_age: far_max,
    }
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
