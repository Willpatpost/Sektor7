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
    Event, EventClass, EventSpec, ExecCtx, Field, Fingerprinter, Schedule, Spine, Tick, World,
};

/// Eight logical region identifiers (Stage 3 metadata for later stages).
pub const REGION_COUNT: usize = 8;

/// The Stage 3 workload: 1,000 stable NPC identities.
pub const NPC_COUNT: usize = 1000;

/// Workload configuration — all policy, all configurable (Stage 3 §2).
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
    /// Executed activity count per NPC, indexed by stable identity.
    steps: Vec<u64>,
}

impl NpcWorld {
    pub fn new(config: NpcConfig) -> Self {
        NpcWorld {
            config,
            steps: vec![0; NPC_COUNT],
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

    /// The lane for a region under the configured classification.
    pub fn region_field(&self, region: usize) -> Field {
        if self.config.near_regions[region] {
            Field::Near
        } else {
            Field::Far
        }
    }
}

/// Installs the initial schedule for every NPC identity: one event per
/// NPC, due within the first two activity periods (seeded spread), in
/// the region's lane. The chain then self-sustains through follow-ups.
pub fn install_initial_schedules(spine: &mut Spine<NpcWorld>, config: &NpcConfig) {
    for npc in 0..NPC_COUNT {
        let region = npc % REGION_COUNT;
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

impl World for NpcWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        let npc = payload_npc(event.payload);
        // Payloads outside the identity space (headless burst/diagnostics
        // injections) are not NPC activities: the spine counts them in
        // its ledger; the workload ignores them.
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
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
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
