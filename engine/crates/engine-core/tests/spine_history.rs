//! PP-B2 — the `spine-history/2` compaction test matrix (the GO §5
//! acceptance list at the engine-core level): the explicit
//! compaction operation and its typed refusals; the compacted round
//! trip and byte determinism; the two-independent-compactions
//! agreement; the prefix-summary chaining across successive
//! compactions; the full-vs-compacted twin continuation equivalence;
//! the version and domain distinctness of the two forms; the
//! corruption matrix over every compacted load gate (through an
//! independent test-only walk of the save/2 layout — the B1
//! reference-codec discipline, so the production decoder is never
//! the only implementation verifying the layout); the accessor and
//! fingerprint contracts (loud panics naming the safe paths, `try_*`
//! safety, the compacted domains' comparison semantics); and the E3
//! complement-of-unfinished membership equivalence against the
//! ledger ground truth.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use common::{
    GoldenWorld, expect_error, golden_spine, patch_u8, patch_u64, restore_golden, scenario,
};
use engine_core::{CompactionError, EventId, Fingerprinter, HistoryKind, Spine, World};

// ------------------------------------------------------------ helpers

fn world_fingerprint(spine: &Spine<GoldenWorld>) -> u64 {
    let mut fp = Fingerprinter::new();
    spine.world().fingerprint(&mut fp);
    fp.finish()
}

/// Asserts a compacted twin and its uncompacted control are
/// logically indistinguishable: authoritative counters, queue
/// depths, both watermark measures, the held fingerprint (valid on
/// both representations), and the world fingerprint — the strongest
/// cross-representation comparisons available (the four
/// ledger-hashing fingerprints are full-history APIs by contract and
/// are asserted unavailable on the twin in the accessor tests).
fn assert_twin_equivalent(control: &Spine<GoldenWorld>, twin: &Spine<GoldenWorld>, context: &str) {
    assert_eq!(control.clock(), twin.clock(), "{context}: clock");
    assert_eq!(control.stats(), twin.stats(), "{context}: stats");
    assert_eq!(
        control.pending_count(),
        twin.pending_count(),
        "{context}: pending"
    );
    assert_eq!(
        control.blocked_count(),
        twin.blocked_count(),
        "{context}: blocked"
    );
    assert_eq!(control.held_count(), twin.held_count(), "{context}: held");
    assert_eq!(
        control.max_executed_due(),
        twin.max_executed_due(),
        "{context}: max executed due (the carried value)"
    );
    assert_eq!(
        control.processed_through(),
        twin.processed_through(),
        "{context}: watermark"
    );
    assert_eq!(
        control.held_fingerprint(),
        twin.held_fingerprint(),
        "{context}: held fingerprint"
    );
    assert_eq!(
        world_fingerprint(control),
        world_fingerprint(twin),
        "{context}: world fingerprint"
    );
    // Conservation, recomputed from the public counters.
    for spine in [control, twin] {
        let stats = spine.stats();
        let conserved = stats.executed
            + spine.pending_count() as u64
            + spine.blocked_count() as u64
            + spine.held_count() as u64;
        assert_eq!(stats.scheduled, conserved, "{context}: conservation");
    }
}

/// The independent test-only walk of a `sektor7/save/2` blob: records
/// the byte offsets of every scalar the corruption tests patch and
/// the tail record positions. Asserts the layout is consumed exactly.
struct V2Walk {
    next_seq_at: usize,
    stats_at: [usize; 7],
    first_unfinished_seq: u64,
    max_present_at: usize,
    max_tick_at: usize,
    summary_len_at: usize,
    summarized_at: usize,
    tail_seq_ats: Vec<usize>,
    tail_late_at: Vec<usize>,
    tail_intended_due: Vec<u64>,
}

fn walk_v2(bytes: &[u8]) -> V2Walk {
    let mut pos = 0usize;
    let mut take_u64 = |pos: &mut usize| -> u64 {
        let value = u64::from_le_bytes(
            bytes[*pos..*pos + 8]
                .try_into()
                .expect("the walk has 8 bytes"),
        );
        *pos += 8;
        value
    };
    fn skip_tag(bytes: &[u8], pos: &mut usize) {
        let len = u64::from_le_bytes(bytes[*pos..*pos + 8].try_into().expect("8 bytes")) as usize;
        *pos += 8 + len;
    }
    fn walk_entries(
        bytes: &[u8],
        pos: &mut usize,
        take_u64: &mut dyn FnMut(&mut usize) -> u64,
    ) -> Option<u64> {
        let count = take_u64(pos) as usize;
        let mut first = None;
        for _ in 0..count {
            let seq = take_u64(pos);
            if first.is_none() {
                first = Some(seq);
            }
            *pos += 4; // class
            *pos += 1; // field
            let kind = bytes[*pos];
            *pos += 1;
            *pos += if kind == 0 { 8 } else { 16 };
            *pos += 8; // payload
            let deps = take_u64(pos) as usize;
            *pos += 8 * deps;
        }
        first
    }
    skip_tag(bytes, &mut pos); // sektor7/save/2
    pos += 4; // version
    skip_tag(bytes, &mut pos); // engine-core/spine-history/2
    take_u64(&mut pos); // clock
    let next_seq_at = pos;
    take_u64(&mut pos); // next_seq
    pos += 4; // budget
    pos += 4; // near_reserve
    let mut stats_at = [0usize; 7];
    for slot in stats_at.iter_mut() {
        *slot = pos;
        pos += 8;
    }
    let near_first = walk_entries(bytes, &mut pos, &mut take_u64);
    let far_first = walk_entries(bytes, &mut pos, &mut take_u64);
    walk_entries(bytes, &mut pos, &mut take_u64); // blocked
    walk_entries(bytes, &mut pos, &mut take_u64); // held
    let max_present_at = pos;
    assert_eq!(
        bytes[pos], 1,
        "the walk: a real compacted save carries the max"
    );
    pos += 1;
    let max_tick_at = pos;
    pos += 8;
    let summary_len_at = pos;
    assert_eq!(take_u64(&mut pos), 32, "the walk: the summary is 32 bytes");
    pos += 32;
    let summarized_at = pos;
    pos += 8;
    let tail_count = take_u64(&mut pos) as usize;
    let mut tail_seq_ats = Vec::with_capacity(tail_count);
    let mut tail_late_at = Vec::with_capacity(tail_count);
    let mut tail_intended_due = Vec::with_capacity(tail_count);
    for _ in 0..tail_count {
        tail_seq_ats.push(pos);
        pos += 8; // seq
        pos += 4; // class
        pos += 1; // field
        tail_intended_due.push(take_u64(&mut pos)); // intended due
        pos += 8; // executed tick
        pos += 1; // schedule kind
        tail_late_at.push(pos);
        pos += 1; // late
    }
    skip_tag(bytes, &mut pos); // world domain
    let world_len = u64::from_le_bytes(bytes[pos..pos + 8].try_into().expect("8 bytes")) as usize;
    pos += 8 + world_len;
    assert_eq!(pos, bytes.len(), "the walk: the save consumed exactly");
    V2Walk {
        next_seq_at,
        stats_at,
        first_unfinished_seq: near_first
            .or(far_first)
            .expect("the scenario has unfinished work"),
        max_present_at,
        max_tick_at,
        summary_len_at,
        summarized_at,
        tail_seq_ats,
        tail_late_at,
        tail_intended_due,
    }
}

/// A compacted save of the golden scenario retaining `retain` records.
fn compacted_bytes(retain: u64) -> Vec<u8> {
    let mut spine = golden_spine();
    spine.compact(retain).expect("the golden scenario compacts");
    spine.save_bytes()
}

// --------------------------------------------------------- the matrix

#[test]
fn compaction_refuses_degenerate_windows() {
    let mut spine = golden_spine();
    let ledger_len = spine.ledger_retained().len() as u64;
    assert!(ledger_len > 0, "the golden scenario executed work");
    assert_eq!(
        spine.compact(ledger_len),
        Err(CompactionError::NothingToSummarize {
            ledger_len,
            retain: ledger_len
        })
    );
    assert_eq!(
        spine.compact(ledger_len + 1),
        Err(CompactionError::RetainExceedsLedger {
            ledger_len,
            retain: ledger_len + 1
        })
    );
    // An empty ledger has nothing to summarize either.
    let mut empty = Spine::new(GoldenWorld::new(), 4);
    assert_eq!(
        empty.compact(0),
        Err(CompactionError::NothingToSummarize {
            ledger_len: 0,
            retain: 0
        })
    );
    assert_eq!(
        empty.compact(1),
        Err(CompactionError::RetainExceedsLedger {
            ledger_len: 0,
            retain: 1
        })
    );
    // The refusals mutate nothing: the spine is still Full, the
    // ledger intact.
    assert_eq!(spine.history_kind(), HistoryKind::Full);
    assert_eq!(spine.ledger_retained().len() as u64, ledger_len);
}

#[test]
fn compacted_round_trip_preserves_logical_state() {
    let control = golden_spine();
    let bytes = compacted_bytes(3);
    // Byte determinism: two encodings of the same state are identical.
    assert_eq!(compacted_bytes(3), bytes);
    let twin = restore_golden(&bytes).expect("the compacted save restores");
    // The restored representation is Compacted with the exact cut.
    assert_eq!(
        twin.history_kind(),
        HistoryKind::Compacted {
            summarized_records: control.ledger_retained().len() as u64 - 3,
            retained_records: 3,
            prefix_summary: match twin.history_kind() {
                HistoryKind::Compacted { prefix_summary, .. } => prefix_summary,
                HistoryKind::Full => unreachable!("asserted above"),
            },
        }
    );
    assert_twin_equivalent(&control, &twin, "the compacted round trip");
    // The retained tail is the control ledger's verbatim suffix.
    assert_eq!(
        twin.ledger_retained(),
        &control.ledger_retained()[control.ledger_retained().len() - 3..]
    );
}

#[test]
fn two_independent_compactions_of_the_same_history_agree() {
    // Two separately constructed instances of the same deterministic
    // scenario, compacted independently — identical representations.
    let a = compacted_bytes(5);
    let b = compacted_bytes(5);
    assert_eq!(a, b, "independent compactions produce identical bytes");
    let spine_a = restore_golden(&a).unwrap();
    let spine_b = restore_golden(&b).unwrap();
    assert_eq!(
        spine_a.compacted_fingerprint(),
        spine_b.compacted_fingerprint()
    );
    assert_eq!(
        spine_a.compacted_state_fingerprint(),
        spine_b.compacted_state_fingerprint()
    );
}

#[test]
fn prefix_summary_chains_across_successive_compactions() {
    // The control runs the full history uncompacted; the twin
    // compacts, restores, continues, and compacts again. The second
    // compaction chains the first's summary, so the final summaries
    // and representations agree with the single equivalent
    // compaction of the same total history.
    let mut control = golden_spine();
    let mut twin = restore_golden(&compacted_bytes(4)).unwrap();
    control.run_ticks(9);
    twin.run_ticks(9);
    assert_twin_equivalent(&control, &twin, "after the continuation");
    // The twin's retained records are exactly the control's records
    // since the cut.
    let cut = match twin.history_kind() {
        HistoryKind::Compacted {
            summarized_records, ..
        } => summarized_records as usize,
        HistoryKind::Full => unreachable!("the twin restored a compacted save"),
    };
    assert_eq!(twin.ledger_retained(), &control.ledger_retained()[cut..]);
    // Summarize everything on both sides.
    control.compact(0).unwrap();
    twin.compact(0).unwrap();
    assert_eq!(
        twin.history_kind(),
        control.history_kind(),
        "the chained compaction reaches the same cut"
    );
    assert_eq!(
        twin.save_bytes(),
        control.save_bytes(),
        "the chained summary and the single equivalent compaction agree"
    );
}

#[test]
fn full_and_compacted_saves_are_version_distinct() {
    let full = golden_spine().save_bytes();
    let compacted = compacted_bytes(3);
    // A save/1 blob with the version field claiming 2 is refused.
    let mut patched = full.clone();
    let version_at = 8 + "sektor7/save/1".len();
    common::patch_u32(&mut patched, version_at, 2);
    assert!(matches!(
        expect_error(&patched),
        engine_core::SaveError::Version { observed: 2, .. }
    ));
    // A save/2 blob with the version field claiming 1 is refused.
    let mut patched = compacted.clone();
    let version_at = 8 + "sektor7/save/2".len();
    common::patch_u32(&mut patched, version_at, 1);
    assert!(matches!(
        expect_error(&patched),
        engine_core::SaveError::Version { observed: 1, .. }
    ));
    // An unknown outer domain is a typed domain error naming the
    // expected tag.
    let mut patched = full.clone();
    patched[8 + "sektor7/save/".len()] = b'9';
    assert!(matches!(
        expect_error(&patched),
        engine_core::SaveError::Domain { .. }
    ));
    // A save/2 blob whose spine section carries the wrong domain is
    // refused by the section gate.
    let mut patched = compacted.clone();
    let spine_tag_at = 8 + "sektor7/save/2".len() + 4 + 8;
    patched[spine_tag_at + "engine-core/spine-history/".len()] = b'9';
    assert!(matches!(
        expect_error(&patched),
        engine_core::SaveError::Domain { .. }
    ));
}

#[test]
fn save_dispatch_selects_the_form() {
    // A Full spine writes save/1; a compacted spine writes save/2 —
    // the first wire field (the length-prefixed domain tag) pins it.
    let full = golden_spine().save_bytes();
    assert_eq!(&full[8..8 + "sektor7/save/1".len()], b"sektor7/save/1");
    let compacted = compacted_bytes(2);
    assert_eq!(&compacted[8..8 + "sektor7/save/2".len()], b"sektor7/save/2");
}

#[test]
fn compacted_corruption_matrix() {
    // Retain all but one record so the tail spans many ticks and
    // includes the scenario's late executions.
    let bytes = compacted_bytes(golden_spine().ledger_retained().len() as u64 - 1);
    assert!(restore_golden(&bytes).is_ok(), "the fixture restores");
    let walk = walk_v2(&bytes);

    let reject = |mutate: &dyn Fn(&mut Vec<u8>)| {
        let mut patched = bytes.clone();
        mutate(&mut patched);
        expect_error(&patched)
    };

    // The empty summary — an explicit compaction summarizes at least
    // one record.
    let error = reject(&|patched: &mut Vec<u8>| {
        patch_u64(patched, walk.summarized_at, 0);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-history/empty-summary")
    );

    // The count identity: summarized + retained == executed.
    let error = reject(&|patched: &mut Vec<u8>| {
        let observed = u64::from_le_bytes(
            patched[walk.summarized_at..walk.summarized_at + 8]
                .try_into()
                .unwrap(),
        );
        patch_u64(patched, walk.summarized_at, observed + 1);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-history/count-identity")
    );

    // The summary must be exactly 32 bytes.
    let error = reject(&|patched: &mut Vec<u8>| {
        patch_u64(patched, walk.summary_len_at, 31);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-history/summary-length")
    );

    // The tail's frontier bound: a record claiming an unallocated seq.
    let error = reject(&|patched: &mut Vec<u8>| {
        let next_seq = u64::from_le_bytes(
            patched[walk.next_seq_at..walk.next_seq_at + 8]
                .try_into()
                .unwrap(),
        );
        patch_u64(patched, walk.tail_seq_ats[0], next_seq);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-save/frontier")
    );

    // The tail's disjointness from unfinished work.
    let error = reject(&|patched: &mut Vec<u8>| {
        patch_u64(patched, walk.tail_seq_ats[0], walk.first_unfinished_seq);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-save/ledger-overlaps-unfinished")
    );

    // A tail record's lateness flag disagreeing with its derivation.
    let error = reject(&|patched: &mut Vec<u8>| {
        let flipped = u8::from(patched[walk.tail_late_at[0]] == 0);
        patch_u8(patched, walk.tail_late_at[0], flipped);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-save/clock-coherence")
    );

    // The late-count bound: the statistic cannot fall below the
    // retained records' own late count.
    let tail_late = walk
        .tail_late_at
        .iter()
        .filter(|&&at| bytes[at] == 1)
        .count() as u64;
    assert!(tail_late > 0, "the scenario's tail includes late records");
    let error = reject(&|patched: &mut Vec<u8>| {
        patch_u64(patched, walk.stats_at[3], tail_late - 1);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-history/late-count-bound")
    );

    // The carried max below the retained tail's maximum intended due.
    let tail_max = walk.tail_intended_due.iter().copied().max().unwrap();
    let error = reject(&|patched: &mut Vec<u8>| {
        patch_u64(patched, walk.max_tick_at, tail_max - 1);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-history/max-below-tail")
    );

    // The carried max beyond the clock.
    let clock = u64::from_le_bytes(
        bytes[walk.max_tick_at - 8 - 8 - 4 - 4 - 8..]
            .iter()
            .take(8)
            .copied()
            .collect::<Vec<u8>>()
            .try_into()
            .unwrap(),
    );
    let error = reject(&|patched: &mut Vec<u8>| {
        patch_u64(patched, walk.max_tick_at, clock + 1);
    });
    assert_eq!(
        error,
        engine_core::SaveError::Integrity("spine-history/max-clock-coherence")
    );

    // A missing carried max: the present flag cleared and the tick
    // bytes removed — a structurally consistent rebuild, so the
    // max-required gate itself fires.
    let mut no_max = bytes[..walk.max_present_at].to_vec();
    no_max.push(0);
    no_max.extend_from_slice(&bytes[walk.max_present_at + 9..]);
    assert_eq!(
        expect_error(&no_max),
        engine_core::SaveError::Integrity("spine-history/max-required")
    );

    // Trailing bytes are corruption.
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
        expect_error(&trailing),
        engine_core::SaveError::Trailing { .. }
    ));

    // Every truncation is rejected — no prefix of a save parses whole.
    for cut in 0..bytes.len() {
        assert!(
            restore_golden(&bytes[..cut]).is_err(),
            "a prefix of {cut} bytes must not parse as a complete save"
        );
    }
}

#[test]
fn accessor_and_fingerprint_contracts() {
    let control = golden_spine();
    let twin = restore_golden(&compacted_bytes(4)).unwrap();

    // The safe paths report unavailability — never a value.
    assert!(twin.try_ledger_full().is_none());
    assert!(twin.try_engine_fingerprint().is_none());
    assert!(twin.try_lane_fingerprint().is_none());
    assert!(twin.try_state_fingerprint().is_none());
    assert!(twin.try_lane_state_fingerprint().is_none());
    // On Full they equal the direct calls.
    assert_eq!(control.try_ledger_full(), Some(control.ledger()));
    assert_eq!(
        control.try_engine_fingerprint(),
        Some(control.engine_fingerprint())
    );

    // The loud paths name the remedy (fail-visible, never silent).
    for (api, remedy) in [
        ("ledger()", "ledger_retained()"),
        ("engine_fingerprint()", "try_engine_fingerprint()"),
        ("state_fingerprint()", "try_state_fingerprint()"),
        ("lane_fingerprint()", "try_lane_fingerprint()"),
        ("lane_state_fingerprint()", "try_lane_state_fingerprint()"),
    ] {
        let result = catch_unwind(AssertUnwindSafe(|| match api {
            "ledger()" => {
                twin.ledger();
            }
            "engine_fingerprint()" => {
                twin.engine_fingerprint();
            }
            "state_fingerprint()" => {
                twin.state_fingerprint();
            }
            "lane_fingerprint()" => {
                twin.lane_fingerprint();
            }
            _ => {
                twin.lane_state_fingerprint();
            }
        }));
        let panic = result.expect_err("the full-history API must surface loudly");
        let message = panic
            .downcast_ref::<String>()
            .expect("the panic payload is a formatted string");
        assert!(message.contains(api), "the panic names {api}");
        assert!(
            message.contains(remedy),
            "the panic names the remedy {remedy}"
        );
    }

    // The compacted domains: two same-cut instances agree...
    let twin_b = restore_golden(&compacted_bytes(4)).unwrap();
    assert_eq!(twin.compacted_fingerprint(), twin_b.compacted_fingerprint());
    assert_eq!(
        twin.compacted_state_fingerprint(),
        twin_b.compacted_state_fingerprint()
    );
    // ...different cuts differ (same logical state, different
    // compaction cut)...
    let other_cut = restore_golden(&compacted_bytes(2)).unwrap();
    assert_ne!(
        twin.compacted_fingerprint(),
        other_cut.compacted_fingerprint()
    );
    // ...and a Full instance cannot compare through them (loudly).
    let result = catch_unwind(AssertUnwindSafe(|| control.compacted_fingerprint()));
    assert!(
        result.is_err(),
        "compacted_fingerprint on Full panics loudly"
    );

    // held_fingerprint hashes no ledger and is valid on both.
    assert_eq!(control.held_fingerprint(), twin.held_fingerprint());
}

#[test]
fn compacted_twin_continuation_equivalence() {
    let mut control = scenario(4242, 6, 2);
    let mut twin = {
        let mut spine = scenario(4242, 6, 2);
        spine.compact(2).unwrap();
        restore_golden(&spine.save_bytes()).unwrap()
    };
    // Pre-continuation equivalence, then identical continuations.
    assert_twin_equivalent(&control, &twin, "pre-continuation");
    control.run_ticks(11);
    twin.run_ticks(11);
    assert_twin_equivalent(&control, &twin, "post-continuation");
    // The twin's retained records are the control's records since the
    // cut — the continuation appended identical records.
    let cut = match twin.history_kind() {
        HistoryKind::Compacted {
            summarized_records, ..
        } => summarized_records as usize,
        HistoryKind::Full => unreachable!("the twin restored a compacted save"),
    };
    assert_eq!(twin.ledger_retained(), &control.ledger_retained()[cut..]);

    // Dependency resolvability after compaction: a follow-up may
    // depend on a summarized (executed) identity, and unknown
    // identities are still rejected.
    let summarized_seq = EventId { seq: 0 };
    assert!(twin.is_executed(summarized_seq));
    let spec = engine_core::EventSpec::new(
        engine_core::EventClass::new(9),
        engine_core::Schedule::AtLogicalTime {
            due: twin.clock().saturating_add(5),
        },
        5,
    )
    .with_dependencies(vec![summarized_seq]);
    assert!(twin.schedule(spec).is_ok());
    let unknown = EventId {
        seq: twin.stats().scheduled + 100,
    };
    let bad = engine_core::EventSpec::new(
        engine_core::EventClass::new(9),
        engine_core::Schedule::AtLogicalTime {
            due: twin.clock().saturating_add(5),
        },
        6,
    )
    .with_dependencies(vec![unknown]);
    assert!(twin.schedule(bad).is_err());
}

#[test]
fn e3_executed_membership_matches_the_ledger_ground_truth() {
    // The complement-of-unfinished representation's membership query
    // equals the ledger's own executed set — the ground truth —
    // across scheduling, execution, suspension, and resumption.
    fn check(spine: &Spine<GoldenWorld>) {
        let Some(ledger) = spine.try_ledger_full() else {
            panic!("the E3 control spine stays Full");
        };
        let executed: std::collections::BTreeSet<u64> =
            ledger.iter().map(|record| record.id.seq).collect();
        let next_seq = spine.stats().scheduled;
        for seq in 0..next_seq {
            assert_eq!(
                spine.is_executed(EventId { seq }),
                executed.contains(&seq),
                "membership mismatch at #{seq}"
            );
        }
    }
    let mut spine = scenario(9001, 7, 3);
    check(&spine);
    spine.run_ticks(4);
    check(&spine);
    spine.suspend_matching(|view| view.payload % 2 == 1);
    check(&spine);
    spine.run_ticks(3);
    check(&spine);
    spine.resume_matching(|_| true);
    check(&spine);
    spine.run_ticks(5);
    check(&spine);
}
