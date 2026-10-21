//! Stage 5 Batch 1 corruption corpus: malformed and intentionally
//! mutated input must fail closed with typed errors — no panic, no
//! undefined behavior, no unbounded allocation, and never a
//! success-shaped value for corrupted state. Mutations are targeted
//! through the independent reference codec's offset map or crafted by
//! its re-serializer, so every documented failure class is exercised
//! against real production bytes.

mod common;

use common::{
    effective_due, expect_error, golden_spine, patch_u8, patch_u32, patch_u64, ref_read, ref_write,
    restore_golden,
};
use engine_core::SaveError;

fn golden_bytes() -> Vec<u8> {
    golden_spine().save_bytes()
}

fn queued_entry(parsed: &common::RefSave) -> common::RefEvent {
    parsed
        .near
        .first()
        .or_else(|| parsed.far.first())
        .expect("the golden scenario queues obligations")
        .clone()
}

#[test]
fn truncation_rejected_at_every_byte_boundary() {
    let bytes = golden_bytes();
    for cut in 0..bytes.len() {
        let result = restore_golden(&bytes[..cut]);
        assert!(
            result.is_err(),
            "a {cut}-byte prefix must not restore (error: {:?})",
            result.err()
        );
    }
    assert!(restore_golden(&bytes).is_ok());
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut bytes = golden_bytes();
    bytes.push(0);
    assert!(matches!(
        expect_error(&bytes),
        SaveError::Trailing { leftover: 1 }
    ));
}

#[test]
fn unsupported_version_is_rejected_by_name() {
    let mut bytes = golden_bytes();
    let parsed = ref_read(&bytes);
    patch_u32(&mut bytes, parsed.version_at, 2);
    assert!(matches!(
        expect_error(&bytes),
        SaveError::Version {
            observed: 2,
            supported: 1
        }
    ));
}

#[test]
fn domain_tag_corruption_is_rejected() {
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);

    let mut header = bytes.clone();
    patch_u8(&mut header, 10, b'x'); // inside "sektor7/save/1"
    assert!(matches!(expect_error(&header), SaveError::Domain { .. }));

    let mut spine_tag = bytes.clone();
    patch_u8(&mut spine_tag, parsed.spine_tag_at + 9, b'x'); // inside "engine-core/spine-save/1"
    assert!(matches!(expect_error(&spine_tag), SaveError::Domain { .. }));

    let mut world = bytes.clone();
    patch_u8(&mut world, parsed.world_tag_at + 9, b'x'); // inside the world domain tag
    assert!(matches!(expect_error(&world), SaveError::Domain { .. }));
}

#[test]
fn invalid_enum_discriminants_are_rejected() {
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);
    let target = queued_entry(&parsed);

    let mut field = bytes.clone();
    patch_u8(&mut field, target.field_at, 2);
    assert!(matches!(expect_error(&field), SaveError::Enum { .. }));

    let mut kind = bytes.clone();
    patch_u8(&mut kind, target.kind_at, 9);
    assert!(matches!(expect_error(&kind), SaveError::Enum { .. }));

    let record = &parsed.ledger[0];
    let mut late = bytes.clone();
    patch_u8(&mut late, record.late_at, 2);
    assert!(matches!(expect_error(&late), SaveError::Enum { .. }));
}

#[test]
fn oversized_counts_are_rejected_before_allocation() {
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);

    // Beyond the documented collection maximum.
    let mut beyond_max = bytes.clone();
    patch_u64(&mut beyond_max, parsed.near_count_at, u64::MAX);
    assert!(matches!(expect_error(&beyond_max), SaveError::Limit { .. }));

    // Under the documented maximum but beyond the bytes remaining —
    // the per-section element bound fires before any allocation.
    let mut impossible = bytes.clone();
    let remaining_after = (bytes.len() - (parsed.near_count_at + 8)) as u64;
    patch_u64(
        &mut impossible,
        parsed.near_count_at,
        remaining_after / 38 + 1,
    );
    assert!(matches!(expect_error(&impossible), SaveError::Limit { .. }));

    // A dependency count inside a queued entry.
    let target = queued_entry(&parsed);
    let mut deps = bytes.clone();
    patch_u64(&mut deps, target.dep_count_at, 1 << 20);
    assert!(matches!(expect_error(&deps), SaveError::Limit { .. }));

    // The world section's byte length.
    let mut world = bytes.clone();
    patch_u64(&mut world, parsed.world_len_at, u64::MAX);
    assert!(matches!(expect_error(&world), SaveError::Limit { .. }));
}

#[test]
fn noncanonical_lane_order_is_rejected() {
    let bytes = golden_bytes();
    let mut parsed = ref_read(&bytes);
    {
        // The far lane is guaranteed two entries by construction (the
        // due-20 cause and the due-25 seed); the near lane's depth
        // depends on how the follow-up budget played out.
        let lane = if parsed.near.len() >= 2 {
            &mut parsed.near
        } else if parsed.far.len() >= 2 {
            &mut parsed.far
        } else {
            panic!("the golden scenario must queue at least two obligations in one lane");
        };
        lane.reverse();
    }
    let mutated = ref_write(&parsed);
    assert!(matches!(expect_error(&mutated), SaveError::Order { .. }));
}

#[test]
fn duplicate_identity_is_rejected() {
    // Cross-section: an identity already present in the far lane
    // injected into the near lane (kept in canonical lane order so the
    // identity gate, not the order gate, fires).
    let bytes = golden_bytes();
    let mut parsed = ref_read(&bytes);
    let mut injected = parsed.far.first().expect("far entry").clone();
    // Correction A's lane-field-coherence gate fires during the section
    // read, so the injected identity carries the target lane's field —
    // the cross-section identity gate is what this test exercises.
    injected.field = 0;
    parsed.near.push(injected);
    parsed
        .near
        .sort_by_key(|event| (effective_due(event), event.seq));
    let mutated = ref_write(&parsed);
    assert!(matches!(
        expect_error(&mutated),
        SaveError::Duplicate { .. }
    ));
}

#[test]
fn duplicate_ledger_identity_is_rejected() {
    let mut bytes = golden_bytes();
    let parsed = ref_read(&bytes);
    assert!(parsed.ledger.len() >= 2);
    patch_u64(&mut bytes, parsed.ledger[1].seq_at, parsed.ledger[0].seq);
    assert!(matches!(
        expect_error(&bytes),
        SaveError::Duplicate {
            section: "spine-save/ledger",
            ..
        }
    ));
}

#[test]
fn contradictory_statistics_are_rejected() {
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);

    // stats.executed off by one: the structural/ledger-count gates.
    let mut executed = bytes.clone();
    patch_u64(&mut executed, parsed.stats_at[1], parsed.stats[1] + 1);
    assert!(matches!(expect_error(&executed), SaveError::Integrity(_)));

    // stats.scheduled off by one: the dense-allocation gate.
    let mut scheduled = bytes.clone();
    patch_u64(&mut scheduled, parsed.stats_at[0], parsed.stats[0] + 1);
    assert!(matches!(expect_error(&scheduled), SaveError::Integrity(_)));

    // A counter beyond its structural bound (a tick can be
    // budget-exhausted at most once per elapsed tick).
    let mut exhausted = bytes.clone();
    patch_u64(&mut exhausted, parsed.stats_at[6], parsed.clock + 1);
    assert!(matches!(
        expect_error(&exhausted),
        SaveError::Integrity("spine-save/counter-bounds")
    ));
}

#[test]
fn fabricated_frontier_counters_are_rejected() {
    // A frontier pushed to u64::MAX with counters kept consistent
    // through the dense-allocation and structural-conservation gates
    // still cannot be real: the full-fidelity ledger count contradicts
    // the claimed executions. (The conservation sums are checked
    // arithmetic; with save/1's invariants they cannot overflow once
    // the preceding gates hold, so a fabricated frontier fails at the
    // next gate instead — the Overflow class is the defensive
    // checked-arithmetic failure for future format evolution.)
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);
    let unfinished =
        (parsed.near.len() + parsed.far.len() + parsed.blocked.len() + parsed.held.len()) as u64;

    let mut fabricated = bytes.clone();
    patch_u64(&mut fabricated, parsed.next_seq_at, u64::MAX);
    patch_u64(&mut fabricated, parsed.stats_at[0], u64::MAX);
    patch_u64(&mut fabricated, parsed.stats_at[1], u64::MAX - unfinished);
    assert!(matches!(
        expect_error(&fabricated),
        SaveError::Integrity("spine-save/ledger-count")
    ));
}

#[test]
fn unnormalized_configuration_is_rejected() {
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);

    let mut zero = bytes.clone();
    patch_u32(&mut zero, parsed.budget_at, 0);
    assert!(matches!(
        expect_error(&zero),
        SaveError::Integrity("spine-save/budget-zero")
    ));

    let mut reserve = bytes.clone();
    patch_u32(&mut reserve, parsed.near_reserve_at, parsed.budget + 1);
    assert!(matches!(
        expect_error(&reserve),
        SaveError::Integrity("spine-save/reserve-exceeds-budget")
    ));
}

#[test]
fn dependency_beyond_the_allocation_frontier_is_rejected() {
    let mut bytes = golden_bytes();
    let parsed = ref_read(&bytes);
    let blocked = parsed.blocked.first().expect("blocked entry");
    assert!(
        !blocked.deps.is_empty(),
        "the golden blocked obligation carries dependencies"
    );
    let first_dep_at = blocked.dep_count_at + 8;
    patch_u64(&mut bytes, first_dep_at, parsed.next_seq);
    assert!(matches!(
        expect_error(&bytes),
        SaveError::Integrity("spine-save/dependency-frontier")
    ));
}

#[test]
fn ledger_overlapping_unfinished_work_is_rejected() {
    let mut bytes = golden_bytes();
    let parsed = ref_read(&bytes);
    let queued = queued_entry(&parsed);
    patch_u64(&mut bytes, parsed.ledger[0].seq_at, queued.seq);
    assert!(matches!(
        expect_error(&bytes),
        SaveError::Integrity("spine-save/ledger-overlaps-unfinished")
    ));
}

#[test]
fn clock_incoherence_is_rejected() {
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);

    // An execution recorded in the future.
    let mut future = bytes.clone();
    patch_u64(
        &mut future,
        parsed.ledger[0].executed_tick_at,
        parsed.clock + 1,
    );
    assert!(matches!(
        expect_error(&future),
        SaveError::Integrity("spine-save/clock-coherence")
    ));

    // The encoded lateness flag contradicting its derivation.
    let on_time = parsed
        .ledger
        .iter()
        .find(|record| record.late == 0)
        .expect("an on-time record");
    let mut flag = bytes.clone();
    patch_u8(&mut flag, on_time.late_at, 1);
    assert!(matches!(
        expect_error(&flag),
        SaveError::Integrity("spine-save/clock-coherence")
    ));
}

#[test]
fn blocked_without_unmet_dependencies_is_rejected() {
    let bytes = golden_bytes();
    let mut parsed = ref_read(&bytes);
    let mut stripped = parsed.blocked[0].clone();
    stripped.deps.clear();
    parsed.blocked[0] = stripped;
    let mutated = ref_write(&parsed);
    assert!(matches!(
        expect_error(&mutated),
        SaveError::Integrity("spine-save/blocked-without-unmet-deps")
    ));
}

#[test]
fn world_section_trailing_bytes_are_rejected() {
    let bytes = golden_bytes();
    let mut parsed = ref_read(&bytes);
    parsed.world_bytes.push(0);
    let mutated = ref_write(&parsed);
    assert!(matches!(expect_error(&mutated), SaveError::Trailing { .. }));
}

#[test]
fn lane_field_mismatch_is_rejected() {
    // Review correction A: a serialized field disagreeing with its
    // containing lane section is not a live state — the spine routes
    // every obligation to the queue matching its field — and is
    // rejected, never silently re-laned.
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);
    let near = parsed.near.first().expect("a near-lane entry");
    let mut far_in_near = bytes.clone();
    patch_u8(&mut far_in_near, near.field_at, 1);
    assert!(matches!(
        expect_error(&far_in_near),
        SaveError::Integrity("spine-save/lane-field-coherence")
    ));
    let far = parsed.far.first().expect("a far-lane entry");
    let mut near_in_far = bytes.clone();
    patch_u8(&mut near_in_far, far.field_at, 0);
    assert!(matches!(
        expect_error(&near_in_far),
        SaveError::Integrity("spine-save/lane-field-coherence")
    ));
}

#[test]
fn late_execution_counter_reconciliation_is_enforced() {
    // Review correction B: the statistic must equal the ledger's count
    // of late records exactly — the individual records unchanged, the
    // statistic mutated in both directions.
    let bytes = golden_bytes();
    let parsed = ref_read(&bytes);
    assert!(parsed.stats[3] > 0, "the golden scenario executes late");
    let mut inflated = bytes.clone();
    patch_u64(&mut inflated, parsed.stats_at[3], parsed.stats[3] + 1);
    assert!(matches!(
        expect_error(&inflated),
        SaveError::Integrity("spine-save/late-execution-count")
    ));
    let mut deflated = bytes.clone();
    patch_u64(&mut deflated, parsed.stats_at[3], parsed.stats[3] - 1);
    assert!(matches!(
        expect_error(&deflated),
        SaveError::Integrity("spine-save/late-execution-count")
    ));
}

#[test]
fn decreasing_execution_tick_is_rejected() {
    // Review correction C: an otherwise structurally consistent ledger
    // whose executed ticks regress. The purpose-built scenario (budget
    // 1, four due-1 obligations) executes one per tick at ticks 1..4;
    // patching the final record's executed tick down to 2 keeps every
    // per-record invariant (due ≤ executed ≤ clock; the lateness flag
    // still matches its derivation — 2 > 1) while breaking the
    // chronological nondecrease.
    let mut spine = engine_core::Spine::with_reservation(common::GoldenWorld::new(), 1, 1);
    for payload in [3u64, 7, 11, 15] {
        spine.schedule(common::at(1, payload)).unwrap();
    }
    spine.run_ticks(4);
    let bytes = spine.save_bytes();
    assert!(
        restore_golden(&bytes).is_ok(),
        "the purpose-built save must be valid before mutation"
    );
    let mut parsed = ref_read(&bytes);
    let last = parsed.ledger.len() - 1;
    assert_eq!(parsed.ledger[last].executed_tick, 4);
    assert_eq!(parsed.ledger[last].late, 1);
    parsed.ledger[last].executed_tick = 2;
    let mutated = ref_write(&parsed);
    assert!(matches!(
        expect_error(&mutated),
        SaveError::Integrity("spine-save/ledger-order")
    ));
}
