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
    LodLevel, SaveError, SaveWorld, Schedule, Spine, Tick, World,
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
