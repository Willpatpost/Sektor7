//! Stage 5 Batch 1 save self-test: a fixed deterministic scenario's
//! canonical save bytes, summarized as length + FNV-1a digest — the
//! cross-process and cross-platform determinism instrument. CI runs
//! this mode twice per platform and pins both values; the full pinned
//! golden bytes live in engine-core's save tests
//! (`tests/save_roundtrip.rs`).

use std::collections::BTreeMap;

use engine_core::wire::MAX_COLLECTION_COUNT;
use engine_core::{
    Decoder, Diagnostics, Encoder, Event, EventClass, EventSpec, ExecCtx, Field, Fingerprinter,
    Level, SaveError, SaveWorld, Schedule, Spine, Tick, World,
};

/// Application-local demo world proving the save-codec boundary with
/// application-owned policy — deliberately not `NpcWorld` (the
/// NpcWorld/RegionDirector integration is Batch 4). Counts executions
/// per payload and spawns a bounded follow-up mix.
struct SaveDemoWorld {
    counts: BTreeMap<u64, u64>,
    spawn_budget: u64,
}

impl World for SaveDemoWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        *self.counts.entry(event.payload).or_insert(0) += 1;
        if self.spawn_budget == 0 {
            return;
        }
        self.spawn_budget -= 1;
        match event.payload % 3 {
            0 => ctx.follow_up(3, EventClass::new(9), event.payload + 3, vec![]),
            1 => ctx.follow_up_in_field(
                if event.field == Field::Far {
                    Field::Near
                } else {
                    Field::Far
                },
                2,
                EventClass::new(9),
                event.payload + 3,
                vec![event.id],
            ),
            _ => {}
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-slice/save-demo-world/1");
        fp.write_usize(self.counts.len());
        for (payload, count) in &self.counts {
            fp.write_u64(*payload);
            fp.write_u64(*count);
        }
        fp.write_u64(self.spawn_budget);
    }
}

impl SaveWorld for SaveDemoWorld {
    const SAVE_DOMAIN: &'static str = "engine-slice/save-demo-world-save/1";

    fn save(&self, enc: &mut Encoder) {
        enc.write_count(self.counts.len());
        for (payload, count) in &self.counts {
            enc.write_u64(*payload);
            enc.write_u64(*count);
        }
        enc.write_u64(self.spawn_budget);
    }

    fn restore(dec: &mut Decoder) -> Result<Self, SaveError> {
        const SECTION: &str = "save-demo-world/counts";
        let count = dec.read_count(MAX_COLLECTION_COUNT, SECTION, "entry-count")?;
        if count > dec.remaining() as u64 / 16 {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "entry-count",
                value: count,
                max: dec.remaining() as u64 / 16,
            });
        }
        let mut counts = BTreeMap::new();
        let mut prev: Option<u64> = None;
        for _ in 0..count {
            let payload = dec.read_u64()?;
            let executions = dec.read_u64()?;
            if let Some(prior) = prev
                && payload <= prior
            {
                return Err(SaveError::Order { section: SECTION });
            }
            prev = Some(payload);
            counts.insert(payload, executions);
        }
        let spawn_budget = dec.read_u64()?;
        Ok(SaveDemoWorld {
            counts,
            spawn_budget,
        })
    }
}

fn demo_at(due: u64, payload: u64) -> EventSpec {
    EventSpec::new(
        EventClass::new(10),
        Schedule::AtLogicalTime { due: Tick(due) },
        payload,
    )
}

/// Builds the fixed scenario and returns the canonical save's byte
/// length and FNV-1a digest (16-character lowercase hex). The restored
/// re-encoding is asserted byte-identical before the values are
/// reported, so the printed pair always witnesses a live round-trip.
pub fn run_save_selftest() -> (u64, String) {
    let mut spine = Spine::with_reservation(
        SaveDemoWorld {
            counts: BTreeMap::new(),
            spawn_budget: 12,
        },
        5,
        2,
    );
    for i in 0..6u64 {
        spine.schedule(demo_at(4 + i, 700 + i)).unwrap();
    }
    for i in 0..4u64 {
        spine
            .schedule(demo_at(5 + i, 800 + i).in_field(Field::Far))
            .unwrap();
    }
    let cause = spine
        .schedule(demo_at(18, 900).in_field(Field::Far))
        .unwrap();
    spine
        .schedule(
            EventSpec::new(
                EventClass::new(11),
                Schedule::AtLogicalTime { due: Tick(5) },
                901,
            )
            .in_field(Field::Near)
            .with_dependencies(vec![cause]),
        )
        .unwrap();
    spine.schedule(demo_at(22, 950)).unwrap();
    spine.run_ticks(8);
    spine.suspend_matching(|view| (800..840).contains(&view.payload));
    spine.schedule_held(demo_at(2, 960)).unwrap();
    spine.run_ticks(5);

    let bytes = spine.save_bytes();
    let restored = Spine::<SaveDemoWorld>::restore(&bytes, Diagnostics::new(Level::Off))
        .expect("the demo save must restore");
    assert_eq!(
        restored.save_bytes(),
        bytes,
        "the restored re-encoding must be byte-identical"
    );
    let mut fp = Fingerprinter::new();
    fp.write_bytes(&bytes);
    (bytes.len() as u64, fp.finish_hex())
}
