//! The Stage 0 acceptance requirement: at least one deterministic headless
//! state-fingerprint test. Runs the same scripted scenario twice from
//! scratch and requires identical engine and state fingerprints — the
//! E-07…E-10 double-run discipline, ported to the engine core.

use std::collections::BTreeMap;

use engine_core::{
    Event, EventClass, EventId, EventSpec, ExecCtx, Fingerprinter, Schedule, Spine, Tick, World,
};

/// xorshift64* — tiny, fully specified, seed-stable. No external RNG
/// dependency (Stage 0 keeps the dependency list empty).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound.max(1)
    }
}

/// Test world in the harness tradition: counters plus a deterministic
/// follow-up policy.
struct ScriptedWorld {
    counters: BTreeMap<u64, u64>,
}

impl World for ScriptedWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        *self.counters.entry(event.payload).or_insert(0) += 1;
        if event.payload.is_multiple_of(3) {
            ctx.follow_up(
                1 + event.payload % 4,
                event.class,
                event.payload + 1,
                Vec::new(),
            );
        }
        if event.payload.is_multiple_of(7) {
            // Follow-up depending on the event currently executing — valid
            // because the spine records execution before the handler runs.
            ctx.follow_up(2, EventClass::new(2), event.payload + 3, vec![event.id]);
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-core-test/scripted-world/1");
        fp.write_usize(self.counters.len());
        for (payload, count) in &self.counters {
            fp.write_u64(*payload);
            fp.write_u64(*count);
        }
    }
}

fn build_scenario(rng: &mut Rng, spine: &mut Spine<ScriptedWorld>) {
    let mut scheduled: Vec<EventId> = Vec::new();
    for i in 0..120u64 {
        let class = EventClass::new(rng.below(4) as u16);
        let due = Tick(1 + rng.below(200));
        let payload = rng.below(64);
        let mut spec = EventSpec::new(class, Schedule::AtLogicalTime { due }, payload);
        if i.is_multiple_of(5) && !scheduled.is_empty() {
            let mut deps = vec![scheduled[rng.below(scheduled.len() as u64) as usize]];
            if i.is_multiple_of(10) && scheduled.len() > 4 {
                deps.push(scheduled[rng.below(scheduled.len() as u64) as usize]);
            }
            spec = spec.with_dependencies(deps);
        }
        if let Ok(id) = spine.schedule(spec) {
            scheduled.push(id);
        }
    }
}

fn run_scenario(seed: u64) -> (u64, u64, engine_core::Stats, usize) {
    let mut rng = Rng::new(seed);
    let mut spine = Spine::new(
        ScriptedWorld {
            counters: BTreeMap::new(),
        },
        16,
    );
    build_scenario(&mut rng, &mut spine);

    // Bursts with late injections between them.
    for burst in 0..4u64 {
        spine.run_ticks(45);
        let late_due = Tick(spine.clock().as_u64().saturating_sub(3));
        spine
            .schedule(EventSpec::new(
                EventClass::new(3),
                Schedule::AtLogicalTime { due: late_due },
                100 + burst,
            ))
            .expect("late injection has no dependencies");
    }
    spine.run_to_completion(2000);

    (
        spine.engine_fingerprint(),
        spine.state_fingerprint(),
        spine.stats(),
        spine.ledger().len(),
    )
}

#[test]
fn double_run_fingerprints_are_identical() {
    let first = run_scenario(1337);
    let second = run_scenario(1337);

    assert_eq!(
        first.0, second.0,
        "engine fingerprints must match across a double run"
    );
    assert_eq!(
        first.1, second.1,
        "state fingerprints must match across a double run"
    );
    assert_eq!(first.2, second.2, "stats must match across a double run");
    assert_eq!(first.3, second.3, "ledger sizes must match");

    // The scenario must genuinely exercise the interesting paths.
    let stats = first.2;
    assert!(stats.scheduled >= 124, "scenario scheduled enough events");
    assert_eq!(stats.late_arrivals, 4, "four late injections occurred");
    assert!(
        stats.late_executions >= 4,
        "late arrivals must execute late: {stats:?}"
    );
    assert_eq!(
        stats.executed, stats.scheduled,
        "no silent drops: every scheduled event executed ({stats:?})"
    );
    assert_eq!(
        stats.invalid_dropped, 0,
        "a valid scenario produces no rejected follow-ups"
    );
}

#[test]
fn fingerprints_are_sensitive_to_scenario_changes() {
    let baseline = run_scenario(1337);
    let perturbed = run_scenario(1338);
    assert_ne!(
        baseline.0, perturbed.0,
        "different scenarios must produce different engine fingerprints"
    );
    assert_ne!(
        baseline.1, perturbed.1,
        "different scenarios must produce different state fingerprints"
    );
}
