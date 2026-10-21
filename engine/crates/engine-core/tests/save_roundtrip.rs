//! Stage 5 Batch 1 save verification: byte-for-byte round-trips,
//! continuation invisibility (a restored spine continued with identical
//! inputs is indistinguishable from the never-saved control), the
//! pinned golden fixture, and the independent reference codec's
//! validation of the production bytes.

mod common;

use common::{
    assert_spines_equivalent, golden_spine, hex, ref_read, ref_write, restore_golden, scenario,
};
use engine_core::Spine;

#[test]
fn golden_state_covers_every_obligation_location() {
    // Coverage proof before round-tripping: the golden scenario must
    // exercise queued work in both lanes, blocked, held, executed,
    // late arrivals and late executions, and budget exhaustion — a
    // round-trip over an empty shell proves nothing.
    let spine = golden_spine();
    assert!(spine.near_pending_count() > 0, "near lane");
    assert!(spine.far_pending_count() > 0, "far lane");
    assert!(spine.blocked_count() > 0, "blocked set");
    assert!(spine.held_count() > 0, "held set");
    assert!(!spine.ledger().is_empty(), "ledger");
    assert!(spine.stats().late_arrivals > 0, "late arrivals");
    assert!(spine.stats().late_executions > 0, "late executions");
    assert!(
        spine.stats().budget_exhausted_ticks > 0,
        "budget exhaustion"
    );
    assert!(spine.processed_through().is_some(), "watermark computable");
}

#[test]
fn empty_spine_round_trips() {
    let spine = Spine::with_reservation(common::GoldenWorld::new(), 4, 2);
    let bytes = spine.save_bytes();
    let restored = restore_golden(&bytes).unwrap();
    assert_eq!(spine.stats().scheduled, 0);
    assert_spines_equivalent(&spine, &restored, "empty spine");
}

#[test]
fn round_trip_is_byte_identical() {
    // encode(decode(encode(state))) == encode(state), byte for byte —
    // not merely equivalent debug output — across the golden scenario
    // and a config/seed spread.
    let mut cases = vec![golden_spine()];
    for (seed, budget, reserve) in [(7u64, 3u32, 1u32), (42, 5, 2), (2026, 8, 4), (99, 2, 1)] {
        cases.push(scenario(seed, budget, reserve));
    }
    for spine in &cases {
        let first = spine.save_bytes();
        let restored = restore_golden(&first).unwrap();
        let second = restored.save_bytes();
        assert_eq!(
            first, second,
            "re-encoding a restored spine must be byte-identical"
        );
        assert_spines_equivalent(spine, &restored, "round trip");
    }
}

/// The pinned golden fixture (wire-format spec §golden-fixtures): the
/// canonical bytes of the fixed golden scenario, identical across
/// platforms, compilers, and processes. CI runs this test on Linux,
/// Windows, and macOS ARM64 — the pinned value is the cross-platform
/// byte-equality proof.
const GOLDEN_HEX: &str = "0e0000000000000073656b746f72372f736176652f31010000001800000000000000656e67696e652d636f72652f7370696e652d736176652f310d000000000000002b0000000000000006000000020000002b00000000000000210000000000000017000000000000001900000000000000010000000000000000000000000000000400000000000000010000000000000010000000000000000300000000001e00000000000000c906000000000000000000000000000002000000000000000e0000000000000003000000010014000000000000000106000000000000000000000000000011000000000000000300000001001900000000000000ca06000000000000000000000000000001000000000000000f000000000000000500000000000600000000000000020600000000000001000000000000000e0000000000000006000000000000000a0000000000000003000000010006000000000000009f0500000000000000000000000000000b000000000000000300000001000700000000000000a00500000000000000000000000000000c000000000000000300000001000800000000000000a10500000000000000000000000000000d000000000000000300000001000900000000000000a2050000000000000000000000000000120000000000000007000000000104000000000000000200000000000000a1050000000000000100000000000000080000000000000025000000000000000300000000000300000000000000650600000000000000000000000000002100000000000000080000000000000003000000010400000000000000040000000000000000000000000000000000030000000005000000000000000500000000000000000009000000000000000300000001050000000000000005000000000000000000140000000000000007000000010000000000000000050000000000000000011500000000000000070000000100000000000000000500000000000000000116000000000000000700000001000000000000000005000000000000000001170000000000000007000000010000000000000000050000000000000000010100000000000000030000000006000000000000000600000000000000000019000000000000000700000000000000000000000006000000000000000001180000000000000007000000010000000000000000060000000000000000011b0000000000000007000000010000000000000000060000000000000000011c0000000000000007000000010000000000000000060000000000000000011d0000000000000007000000010000000000000000060000000000000000011a0000000000000007000000000000000000000000070000000000000000011f0000000000000007000000000000000000000000070000000000000000011e00000000000000070000000100000000000000000700000000000000000121000000000000000700000001000000000000000007000000000000000001220000000000000007000000010000000000000000070000000000000000012300000000000000070000000100000000000000000700000000000000000120000000000000000700000000000000000000000008000000000000000001260000000000000007000000000000000000000000080000000000000000012400000000000000070000000100000000000000000800000000000000000128000000000000000700000001000000000000000008000000000000000001290000000000000007000000010000000000000000080000000000000000012a0000000000000007000000010000000000000000080000000000000000012700000000000000070000000000000000000000000900000000000000000102000000000000000300000000070000000000000009000000000000000001130000000000000007000000010700000000000000090000000000000001010300000000000000030000000008000000000000000900000000000000000104000000000000000300000000090000000000000009000000000000000000050000000000000003000000000a000000000000000a000000000000000000060000000000000003000000000b000000000000000b000000000000000000070000000000000003000000000c000000000000000c0000000000000000002200000000000000656e67696e652d636f72652d746573742f736176652d776f726c642d736176652f3100020000000000001f00000000000000390500000000000001000000000000003a0500000000000001000000000000003b0500000000000001000000000000003c0500000000000001000000000000003d0500000000000002000000000000003e0500000000000002000000000000003f0500000000000001000000000000004005000000000000010000000000000042050000000000000100000000000000460500000000000001000000000000004a0500000000000001000000000000004e050000000000000100000000000000520500000000000001000000000000009d0500000000000001000000000000009e050000000000000100000000000000a2050000000000000100000000000000a6050000000000000100000000000000aa050000000000000100000000000000ae050000000000000100000000000000b2050000000000000100000000000000b6050000000000000100000000000000ba050000000000000100000000000000be050000000000000100000000000000c2050000000000000100000000000000c6050000000000000100000000000000ca050000000000000100000000000000ce050000000000000100000000000000d2050000000000000100000000000000d6050000000000000100000000000000da050000000000000100000000000000de0500000000000001000000000000000000000000000000";

#[test]
fn golden_save_bytes_are_pinned() {
    let bytes = golden_spine().save_bytes();
    let actual = hex(&bytes);
    assert_eq!(actual, GOLDEN_HEX);
}

#[test]
fn reference_codec_round_trips_production_bytes() {
    // The independent reference codec (no production Decoder) must
    // parse the production encoder's output — asserting the spec's
    // layout and canonical properties along the way — and reproduce
    // it exactly on re-serialization: a second implementation
    // verifying the first (B1 authorization §6.E).
    let bytes = golden_spine().save_bytes();
    let parsed = ref_read(&bytes);
    assert_eq!(
        ref_write(&parsed),
        bytes,
        "the independent reference codec must reproduce the production bytes exactly"
    );
}

#[test]
fn continuation_invisibility_golden() {
    // Save at an intermediate tick, restore into an independent
    // instance, continue both with identical inputs: scheduling
    // decisions, executions, statistics, watermarks, fingerprints,
    // and re-encoded bytes must all agree.
    let mut control = golden_spine();
    let mut restored = restore_golden(&control.save_bytes()).unwrap();
    control.run_ticks(25);
    restored.run_ticks(25);
    assert_spines_equivalent(&control, &restored, "golden continuation");
}

#[test]
fn continuation_invisibility_across_configs() {
    for (seed, budget, reserve) in [(7u64, 3u32, 1u32), (42, 5, 2), (2026, 8, 4)] {
        let mut control = scenario(seed, budget, reserve);
        let mut restored = restore_golden(&control.save_bytes()).unwrap();
        control.run_ticks(30);
        restored.run_ticks(30);
        assert_spines_equivalent(&control, &restored, "config continuation");
    }
}

#[test]
fn continuation_resolves_cross_lane_causality_after_restore() {
    // The golden scenario's near dependent (due 6) waits on a far
    // cause due 20 — the dependency resolves only after the restore
    // boundary, proving the waiter-index reconstruction and the
    // blocked→queue release path.
    let mut control = golden_spine();
    let mut restored = restore_golden(&control.save_bytes()).unwrap();
    assert_eq!(control.blocked_count(), 1);
    control.run_ticks(20); // clock 33: the cause executes, the dependent unblocks and runs late
    restored.run_ticks(20);
    assert_eq!(
        control.blocked_count(),
        0,
        "the dependent must have unblocked and executed"
    );
    assert_spines_equivalent(&control, &restored, "cross-lane causality");
}

#[test]
fn continuation_with_held_reactivation_and_overdue_work() {
    // Reactivate the same held obligations on both sides with identical
    // predicates — the directly-held entry is overdue (due 3 at clock
    // 13), so its resumed execution is late: held→queue re-entry,
    // lateness accounting, and the held index all cross the restore
    // boundary.
    let mut control = golden_spine();
    let mut restored = restore_golden(&control.save_bytes()).unwrap();
    let control_resumed = control.resume_matching(|view| view.payload >= 1500);
    let restored_resumed = restored.resume_matching(|view| view.payload >= 1500);
    assert_eq!(control_resumed, restored_resumed);
    assert!(
        control_resumed > 0,
        "the scenario must hold matching obligations"
    );
    control.run_ticks(12);
    restored.run_ticks(12);
    assert_spines_equivalent(&control, &restored, "held reactivation");
}

#[test]
fn repeated_save_load_cycles_stay_invisible() {
    let mut control = golden_spine();
    let mut restored = restore_golden(&control.save_bytes()).unwrap();
    for cycle in 0..3 {
        control.run_ticks(4);
        restored.run_ticks(4);
        let bytes = restored.save_bytes();
        restored = restore_golden(&bytes).unwrap();
        assert_eq!(
            restored.save_bytes(),
            bytes,
            "cycle {cycle}: re-encode stability"
        );
    }
    control.run_ticks(10);
    restored.run_ticks(10);
    assert_spines_equivalent(&control, &restored, "repeated cycles");
}

#[test]
fn duplicate_dependencies_round_trip_faithfully() {
    // Dependencies are stored-order faithful on the wire — a
    // documented exception to canonical section ordering: they are
    // semantically a set, the fingerprints hash the stored vec, and
    // the live spine accepts duplicates — so a save must neither
    // reorder nor deduplicate them.
    let mut spine = Spine::with_reservation(common::GoldenWorld::new(), 8, 4);
    let first = spine.schedule(common::at(3, 500)).unwrap();
    spine
        .schedule(common::at(5, 501).with_dependencies(vec![first, first, first]))
        .unwrap();
    spine.run_ticks(5);
    let bytes = spine.save_bytes();
    let restored = restore_golden(&bytes).unwrap();
    assert_spines_equivalent(&spine, &restored, "duplicate deps");
    assert_eq!(bytes, restored.save_bytes());
}
