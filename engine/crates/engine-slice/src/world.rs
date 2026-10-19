//! The slice application's demo world.
//!
//! Deliberately simple, deterministic game-side policy illustrating the
//! engine/game boundary (P5): this code lives *outside* the reusable
//! engine, in whatever form the game chooses — here, plain code.

use std::collections::BTreeMap;

use engine_core::{Event, EventClass, ExecCtx, Fingerprinter, World};

pub struct DemoWorld {
    /// payload -> executions
    counters: BTreeMap<u64, u64>,
    /// execution log: (seq, executed tick)
    log: Vec<(u64, u64)>,
}

impl DemoWorld {
    pub fn new() -> Self {
        DemoWorld {
            counters: BTreeMap::new(),
            log: Vec::new(),
        }
    }

    pub fn counters(&self) -> &BTreeMap<u64, u64> {
        &self.counters
    }

    pub fn log(&self) -> &[(u64, u64)] {
        &self.log
    }

    pub fn total_executions(&self) -> u64 {
        self.log.len() as u64
    }
}

impl Default for DemoWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl World for DemoWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        *self.counters.entry(event.payload).or_insert(0) += 1;
        self.log.push((event.id.seq, ctx.executed_tick().as_u64()));

        // Deterministic demo policy:
        // - payload divisible by 3: execution-relative follow-up after 1..=4 ticks
        // - payload divisible by 5: fixed logical-time event 7 ticks ahead
        // - payload divisible by 7: follow-up chained on the currently
        //   executing event (valid: the spine records execution before the
        //   handler)
        if event.payload.is_multiple_of(3) {
            ctx.follow_up(
                1 + event.payload % 4,
                event.class,
                event.payload + 1,
                Vec::new(),
            );
        }
        if event.payload.is_multiple_of(5) {
            ctx.schedule_at(
                ctx.executed_tick().saturating_add(7),
                EventClass::new(1),
                event.payload + 2,
                Vec::new(),
            );
        }
        if event.payload.is_multiple_of(7) {
            ctx.follow_up(2, EventClass::new(2), event.payload + 3, vec![event.id]);
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-slice/demo-world/1");
        fp.write_usize(self.log.len());
        for (seq, tick) in &self.log {
            fp.write_u64(*seq);
            fp.write_u64(*tick);
        }
        fp.write_usize(self.counters.len());
        for (payload, count) in &self.counters {
            fp.write_u64(*payload);
            fp.write_u64(*count);
        }
    }
}
