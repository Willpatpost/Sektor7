//! Shared support for the Stage 5 Batch 1 save tests: the golden test
//! world and scenarios, and an independent test-only reference codec
//! for the save layout. The reference codec exists so the production
//! encoder and decoder are never the only implementations verifying one
//! another (B1 authorization §6.E) — it walks the bytes with its own
//! cursor, asserts the wire-format spec's layout and canonical
//! properties, and can re-serialize a (possibly mutated) parse, which
//! is also how the corruption tests craft targeted structural
//! violations.
//!
//! `#![allow(dead_code)]`: each test binary compiles this module
//! independently and uses a different subset of it.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use engine_core::wire::MAX_COLLECTION_COUNT;
use engine_core::{
    Decoder, Diagnostics, Encoder, Event, EventClass, EventSpec, ExecCtx, Field, Fingerprinter,
    Level, SaveError, SaveWorld, Schedule, Spine, Tick, World,
};

pub const SAVE_DOMAIN: &str = "sektor7/save/1";
pub const SPINE_DOMAIN: &str = "engine-core/spine-save/1";
pub const WORLD_SAVE_DOMAIN: &str = "engine-core-test/save-world-save/1";
pub const SAVE_FORMAT_VERSION: u32 = 1;

/// Total follow-ups the golden world will spawn — bounds every
/// scenario deterministically (the counter is world state, so it
/// serializes and fingerprints with it).
const SPAWN_BUDGET: u64 = 24;

// ------------------------------------------------------------ test world

/// Deterministic test world: counts executions per payload and spawns
/// a bounded, varied mix of follow-ups — execution-relative chains,
/// lane-flipping follow-ups (carrying a dependency on their
/// just-executed cause), and past-due late arrivals — until the spawn
/// budget is spent, so every scenario is finite and deterministic.
pub struct GoldenWorld {
    pub counts: BTreeMap<u64, u64>,
    pub spawn_budget: u64,
}

impl GoldenWorld {
    pub fn new() -> Self {
        GoldenWorld {
            counts: BTreeMap::new(),
            spawn_budget: SPAWN_BUDGET,
        }
    }
}

impl World for GoldenWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        *self.counts.entry(event.payload).or_insert(0) += 1;
        if self.spawn_budget == 0 {
            return;
        }
        self.spawn_budget -= 1;
        let class = EventClass::new(7);
        let payload = event.payload;
        match payload % 4 {
            0 => ctx.follow_up(3, class, payload + 4, vec![]),
            1 => {
                let flipped = if event.field == Field::Far {
                    Field::Near
                } else {
                    Field::Far
                };
                ctx.follow_up_in_field(flipped, 2, class, payload + 4, vec![event.id]);
            }
            2 => ctx.schedule_at(Tick(0), class, payload + 4, vec![]),
            _ => {}
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-core-test/save-world/1");
        fp.write_usize(self.counts.len());
        for (payload, count) in &self.counts {
            fp.write_u64(*payload);
            fp.write_u64(*count);
        }
        fp.write_u64(self.spawn_budget);
    }
}

impl SaveWorld for GoldenWorld {
    const SAVE_DOMAIN: &'static str = WORLD_SAVE_DOMAIN;

    fn save(&self, enc: &mut Encoder) {
        enc.write_count(self.counts.len());
        for (payload, count) in &self.counts {
            enc.write_u64(*payload);
            enc.write_u64(*count);
        }
        enc.write_u64(self.spawn_budget);
    }

    fn restore(dec: &mut Decoder) -> Result<Self, SaveError> {
        const SECTION: &str = "save-world/counts";
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
        Ok(GoldenWorld {
            counts,
            spawn_budget,
        })
    }
}

// ------------------------------------------------------------ scenarios

pub fn at(due: u64, payload: u64) -> EventSpec {
    EventSpec::new(
        EventClass::new(3),
        Schedule::AtLogicalTime { due: Tick(due) },
        payload,
    )
}

/// A parameterized deterministic scenario: near and far lane spreads,
/// a cross-lane causal dependency whose far cause is due after the
/// save point (so the near dependent stays blocked through the save),
/// a far-in-the-future queued obligation in each lane, saturation
/// under the given budget, suspension of far-seeded payloads into
/// held, and a directly-held overdue obligation (late on arrival,
/// late again on resumed execution). The golden scenario is
/// `scenario(1337, 6, 2)`.
pub fn scenario(seed: u64, budget: u32, near_reserve: u32) -> Spine<GoldenWorld> {
    let mut spine = Spine::with_reservation(GoldenWorld::new(), budget, near_reserve);
    for i in 0..8u64 {
        spine.schedule(at(5 + i, seed + i)).unwrap();
    }
    for i in 0..6u64 {
        spine
            .schedule(at(4 + i, seed + 100 + i).in_field(Field::Far))
            .unwrap();
    }
    let cause = spine
        .schedule(at(20, seed + 200).in_field(Field::Far))
        .unwrap();
    spine
        .schedule(
            EventSpec::new(
                EventClass::new(5),
                Schedule::AtLogicalTime { due: Tick(6) },
                seed + 201,
            )
            .in_field(Field::Near)
            .with_dependencies(vec![cause]),
        )
        .unwrap();
    spine.schedule(at(30, seed + 400)).unwrap();
    spine
        .schedule(at(25, seed + 401).in_field(Field::Far))
        .unwrap();
    spine.run_ticks(7);
    spine.suspend_matching(|view| (seed + 100..seed + 140).contains(&view.payload));
    spine.schedule_held(at(3, seed + 300)).unwrap();
    spine.run_ticks(6);
    spine
}

pub fn golden_spine() -> Spine<GoldenWorld> {
    scenario(1337, 6, 2)
}

pub fn restore_golden(bytes: &[u8]) -> Result<Spine<GoldenWorld>, SaveError> {
    Spine::restore(bytes, Diagnostics::new(Level::Off))
}

/// Restores and requires failure — the corruption tests' helper
/// (`Result::unwrap_err` would demand `Spine: Debug`, which the spine
/// deliberately does not implement).
pub fn expect_error(bytes: &[u8]) -> SaveError {
    match restore_golden(bytes) {
        Err(error) => error,
        Ok(_) => panic!("a corrupted save must not restore"),
    }
}

/// Asserts two spines are logically indistinguishable: authoritative
/// counters, the full ledger, queue/blocked/held contents (through the
/// five fingerprint domains), both watermark measures, and — the
/// strongest form — byte-identical re-encoding.
pub fn assert_spines_equivalent(a: &Spine<GoldenWorld>, b: &Spine<GoldenWorld>, context: &str) {
    assert_eq!(a.clock(), b.clock(), "{context}: clock");
    assert_eq!(a.stats(), b.stats(), "{context}: stats");
    assert_eq!(a.ledger(), b.ledger(), "{context}: ledger");
    assert_eq!(
        a.near_pending_count(),
        b.near_pending_count(),
        "{context}: near lane"
    );
    assert_eq!(
        a.far_pending_count(),
        b.far_pending_count(),
        "{context}: far lane"
    );
    assert_eq!(a.blocked_count(), b.blocked_count(), "{context}: blocked");
    assert_eq!(a.held_count(), b.held_count(), "{context}: held");
    assert_eq!(
        a.max_executed_due(),
        b.max_executed_due(),
        "{context}: max executed due"
    );
    assert_eq!(
        a.processed_through(),
        b.processed_through(),
        "{context}: watermark"
    );
    assert_eq!(
        a.engine_fingerprint(),
        b.engine_fingerprint(),
        "{context}: engine fingerprint"
    );
    assert_eq!(
        a.state_fingerprint(),
        b.state_fingerprint(),
        "{context}: state fingerprint"
    );
    assert_eq!(
        a.lane_fingerprint(),
        b.lane_fingerprint(),
        "{context}: lane fingerprint"
    );
    assert_eq!(
        a.lane_state_fingerprint(),
        b.lane_state_fingerprint(),
        "{context}: lane-state fingerprint"
    );
    assert_eq!(
        a.held_fingerprint(),
        b.held_fingerprint(),
        "{context}: held fingerprint"
    );
    assert_eq!(a.save_bytes(), b.save_bytes(), "{context}: save bytes");
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ------------------------------------------------------ reference codec
//
// Independent, test-only implementation of the save layout
// (wire-format spec): its own cursor, no production Decoder. It
// asserts the spec's layout and canonical properties while recording
// the byte offsets the mutation tests patch, and `ref_write`
// re-serializes a (possibly mutated) parse.

struct RefCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> RefCursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        RefCursor { buf, pos: 0 }
    }

    fn pos(&self) -> usize {
        self.pos
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, want: usize) -> &'a [u8] {
        assert!(
            self.remaining() >= want,
            "reference walk ran past the buffer"
        );
        let slice = &self.buf[self.pos..self.pos + want];
        self.pos += want;
        slice
    }

    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }

    fn u32(&mut self) -> u32 {
        let b = self.take(4);
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    fn u64(&mut self) -> u64 {
        let b = self.take(8);
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    }

    fn tag(&mut self, expected: &str) {
        let len = self.u64() as usize;
        let bytes = self.take(len);
        let observed = std::str::from_utf8(bytes).expect("reference walk: tag is UTF-8");
        assert_eq!(observed, expected, "reference walk: unexpected domain tag");
    }

    fn raw_tag(&mut self) -> String {
        let len = self.u64() as usize;
        String::from_utf8(self.take(len).to_vec()).expect("reference walk: tag is UTF-8")
    }
}

/// One parsed obligation entry, with the byte offsets the mutation
/// tests patch.
#[derive(Debug, Clone)]
pub struct RefEvent {
    pub seq: u64,
    pub seq_at: usize,
    pub class: u32,
    pub field: u8,
    pub field_at: usize,
    pub kind: u8,
    pub kind_at: usize,
    pub due: u64,
    pub origin: u64,
    pub delay: u64,
    pub payload: u64,
    pub dep_count_at: usize,
    pub deps: Vec<u64>,
}

/// One parsed execution record.
#[derive(Debug, Clone)]
pub struct RefRecord {
    pub seq: u64,
    pub seq_at: usize,
    pub class: u32,
    pub field: u8,
    pub intended_due: u64,
    pub executed_tick: u64,
    pub executed_tick_at: usize,
    pub kind: u8,
    pub late: u8,
    pub late_at: usize,
}

/// A complete independent parse of a save, with mutation-target
/// offsets.
#[derive(Debug, Clone)]
pub struct RefSave {
    pub version: u32,
    pub version_at: usize,
    pub spine_tag_at: usize,
    pub clock: u64,
    pub clock_at: usize,
    pub next_seq: u64,
    pub next_seq_at: usize,
    pub budget: u32,
    pub budget_at: usize,
    pub near_reserve: u32,
    pub near_reserve_at: usize,
    pub stats: [u64; 7],
    pub stats_at: [usize; 7],
    pub near: Vec<RefEvent>,
    pub near_count_at: usize,
    pub far: Vec<RefEvent>,
    pub far_count_at: usize,
    pub blocked: Vec<RefEvent>,
    pub blocked_count_at: usize,
    pub held: Vec<RefEvent>,
    pub held_count_at: usize,
    pub ledger: Vec<RefRecord>,
    pub ledger_count_at: usize,
    pub world_tag_at: usize,
    pub world_domain: String,
    pub world_len_at: usize,
    pub world_bytes: Vec<u8>,
}

/// The effective due of a parsed entry (the wire spec's derivation).
pub fn effective_due(event: &RefEvent) -> u64 {
    if event.kind == 0 {
        event.due
    } else {
        event.origin.saturating_add(event.delay)
    }
}

pub fn ref_read(bytes: &[u8]) -> RefSave {
    let mut c = RefCursor::new(bytes);
    c.tag(SAVE_DOMAIN);
    let version_at = c.pos();
    let version = c.u32();
    assert_eq!(version, SAVE_FORMAT_VERSION, "reference walk: version");
    let spine_tag_at = c.pos();
    c.tag(SPINE_DOMAIN);
    let clock_at = c.pos();
    let clock = c.u64();
    let next_seq_at = c.pos();
    let next_seq = c.u64();
    let budget_at = c.pos();
    let budget = c.u32();
    let near_reserve_at = c.pos();
    let near_reserve = c.u32();
    let mut stats = [0u64; 7];
    let mut stats_at = [0usize; 7];
    for slot in 0..7 {
        stats_at[slot] = c.pos();
        stats[slot] = c.u64();
    }
    let (near_count_at, near) = ref_events(&mut c);
    let (far_count_at, far) = ref_events(&mut c);
    let (blocked_count_at, blocked) = ref_events(&mut c);
    let (held_count_at, held) = ref_events(&mut c);
    let (ledger_count_at, ledger) = ref_records(&mut c);
    let world_tag_at = c.pos();
    let world_domain = c.raw_tag();
    let world_len_at = c.pos();
    let world_len = c.u64() as usize;
    let world_bytes = c.take(world_len).to_vec();
    assert_eq!(c.remaining(), 0, "reference walk: trailing bytes");

    // The canonical properties the spec requires — asserted here by a
    // second implementation, not only by the production decoder.
    let mut seen = BTreeSet::new();
    for lane in [&near, &far] {
        let mut prev: Option<(u64, u64)> = None;
        for event in lane {
            let key = (effective_due(event), event.seq);
            if let Some(prior) = prev {
                assert!(key > prior, "reference walk: lane entries must ascend");
            }
            prev = Some(key);
            assert!(seen.insert(event.seq), "reference walk: identity reused");
        }
    }
    for section in [&blocked, &held] {
        for pair in section.windows(2) {
            assert!(pair[0].seq < pair[1].seq, "reference walk: section order");
        }
        for event in section {
            assert!(seen.insert(event.seq), "reference walk: identity reused");
        }
    }
    for record in &ledger {
        assert!(
            seen.insert(record.seq),
            "reference walk: executed not disjoint"
        );
    }

    RefSave {
        version,
        version_at,
        spine_tag_at,
        clock,
        clock_at,
        next_seq,
        next_seq_at,
        budget,
        budget_at,
        near_reserve,
        near_reserve_at,
        stats,
        stats_at,
        near,
        near_count_at,
        far,
        far_count_at,
        blocked,
        blocked_count_at,
        held,
        held_count_at,
        ledger,
        ledger_count_at,
        world_tag_at,
        world_domain,
        world_len_at,
        world_bytes,
    }
}

fn ref_events(c: &mut RefCursor) -> (usize, Vec<RefEvent>) {
    let count_at = c.pos();
    let count = c.u64() as usize;
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        let seq_at = c.pos();
        let seq = c.u64();
        let class = c.u32();
        let field_at = c.pos();
        let field = c.u8();
        let kind_at = c.pos();
        let kind = c.u8();
        let (due, origin, delay) = match kind {
            0 => (c.u64(), 0, 0),
            1 => (0, c.u64(), c.u64()),
            other => panic!("reference walk: unknown schedule kind {other}"),
        };
        let payload = c.u64();
        let dep_count_at = c.pos();
        let dep_count = c.u64() as usize;
        let mut deps = Vec::with_capacity(dep_count);
        for _ in 0..dep_count {
            deps.push(c.u64());
        }
        events.push(RefEvent {
            seq,
            seq_at,
            class,
            field,
            field_at,
            kind,
            kind_at,
            due,
            origin,
            delay,
            payload,
            dep_count_at,
            deps,
        });
    }
    (count_at, events)
}

fn ref_records(c: &mut RefCursor) -> (usize, Vec<RefRecord>) {
    let count_at = c.pos();
    let count = c.u64() as usize;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let seq_at = c.pos();
        let seq = c.u64();
        let class = c.u32();
        let field = c.u8();
        let intended_due = c.u64();
        let executed_tick_at = c.pos();
        let executed_tick = c.u64();
        let kind = c.u8();
        let late_at = c.pos();
        let late = c.u8();
        records.push(RefRecord {
            seq,
            seq_at,
            class,
            field,
            intended_due,
            executed_tick,
            executed_tick_at,
            kind,
            late,
            late_at,
        });
    }
    (count_at, records)
}

pub fn ref_write(save: &RefSave) -> Vec<u8> {
    let mut out = Vec::new();
    write_tag(&mut out, SAVE_DOMAIN);
    out.extend_from_slice(&save.version.to_le_bytes());
    write_tag(&mut out, SPINE_DOMAIN);
    out.extend_from_slice(&save.clock.to_le_bytes());
    out.extend_from_slice(&save.next_seq.to_le_bytes());
    out.extend_from_slice(&save.budget.to_le_bytes());
    out.extend_from_slice(&save.near_reserve.to_le_bytes());
    for value in save.stats {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for section in [&save.near, &save.far, &save.blocked, &save.held] {
        out.extend_from_slice(&(section.len() as u64).to_le_bytes());
        for event in section {
            write_event(&mut out, event);
        }
    }
    out.extend_from_slice(&(save.ledger.len() as u64).to_le_bytes());
    for record in &save.ledger {
        out.extend_from_slice(&record.seq.to_le_bytes());
        out.extend_from_slice(&record.class.to_le_bytes());
        out.push(record.field);
        out.extend_from_slice(&record.intended_due.to_le_bytes());
        out.extend_from_slice(&record.executed_tick.to_le_bytes());
        out.push(record.kind);
        out.push(record.late);
    }
    write_tag(&mut out, &save.world_domain);
    out.extend_from_slice(&(save.world_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&save.world_bytes);
    out
}

fn write_tag(out: &mut Vec<u8>, tag: &str) {
    out.extend_from_slice(&(tag.len() as u64).to_le_bytes());
    out.extend_from_slice(tag.as_bytes());
}

fn write_event(out: &mut Vec<u8>, event: &RefEvent) {
    out.extend_from_slice(&event.seq.to_le_bytes());
    out.extend_from_slice(&event.class.to_le_bytes());
    out.push(event.field);
    out.push(event.kind);
    match event.kind {
        0 => out.extend_from_slice(&event.due.to_le_bytes()),
        1 => {
            out.extend_from_slice(&event.origin.to_le_bytes());
            out.extend_from_slice(&event.delay.to_le_bytes());
        }
        other => panic!("reference write: unknown schedule kind {other}"),
    }
    out.extend_from_slice(&event.payload.to_le_bytes());
    out.extend_from_slice(&(event.deps.len() as u64).to_le_bytes());
    for dep in &event.deps {
        out.extend_from_slice(&dep.to_le_bytes());
    }
}

// ------------------------------------------------------------ patching

pub fn patch_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn patch_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub fn patch_u8(bytes: &mut [u8], offset: usize, value: u8) {
    bytes[offset] = value;
}
