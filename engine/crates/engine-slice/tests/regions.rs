//! Stage 4 Batch 2 region contracts (authorization §9 A–H subset,
//! reviewer priorities A–C): presence-driven LOD across all levels,
//! true dormancy (zero ordinary work), identity and conservation
//! through transition cycles, capture exactly-once, reconstruction
//! labels + adaptive selection, watermark safety during dormancy,
//! immediate near-lane protection on reactivation, and determinism
//! across double runs.

use engine_core::{EventClass, EventSpec, Field, LodLevel, PresenceSample, Schedule, Tick};
use engine_slice::npc::{NPC_COUNT, NpcConfig, REGION_COUNT};
use engine_slice::region::{
    CaptureLedger, CaptureStatus, PresenceScenario, ReconstructionMethod, RegionConfig,
    stage4_state_fingerprint_hex,
};
use engine_slice::sim::{SimConfig, SimDriver};

fn region_driver(presence: PresenceScenario) -> SimDriver {
    let region_config = RegionConfig::default();
    let initial = [presence.sample_at(0, &region_config.centers)];
    SimDriver::with_regions(
        NpcConfig::default(),
        SimConfig::default(),
        region_config,
        &initial,
    )
    .expect("valid region config")
}

fn conservation(driver: &SimDriver) -> bool {
    let spine = driver.spine();
    let stats = spine.stats();
    stats.scheduled
        == stats.executed
            + spine.pending_count() as u64
            + spine.blocked_count() as u64
            + spine.held_count() as u64
}

#[test]
fn stationary_presence_assigns_levels_and_true_dormancy() {
    // Authorization §9 A/B: presence drives the ladder; a dormant
    // region performs ZERO ordinary work — its obligations are
    // spine-held, its handler never runs, its NPCs' steps freeze, and
    // the total work drops measurably.
    let mut driver = region_driver(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    driver.run_headless(300);
    let levels = driver.director().expect("director").levels();
    assert_eq!(levels[0], LodLevel::Full, "the occupied region");
    assert_eq!(levels[1], LodLevel::EventDriven, "a diagonal neighbor");
    assert_eq!(levels[4], LodLevel::Dormant, "the far side");
    // Five dormant regions (the ring geometry puts them beyond the
    // event-exit threshold), 125 obligations each, all held.
    assert_eq!(driver.spine().held_count(), 5 * 125);
    // Zero executions for a dormant region — ever (the deactivation
    // preceded the first tick).
    let class_4_executions = driver
        .spine()
        .ledger()
        .iter()
        .filter(|record| record.class.0 == 4)
        .count();
    assert_eq!(class_4_executions, 0, "dormant regions execute nothing");
    // Steps frozen for the dormant region's NPCs.
    for npc in (0..NPC_COUNT).filter(|npc| npc % REGION_COUNT == 4).take(5) {
        assert_eq!(driver.spine().world().steps()[npc], 0);
    }
    // Every NPC's chain is intact: exactly one obligation per NPC —
    // queued + blocked + held == 1000, the no-loss proof.
    let spine = driver.spine();
    assert_eq!(
        spine.pending_count() + spine.blocked_count() + spine.held_count(),
        NPC_COUNT
    );
    assert!(conservation(&driver));
    assert!(
        spine.stats().executed < 20_000,
        "dormancy measurably reduces total work ({})",
        spine.stats().executed
    );
}

#[test]
fn tour_preserves_identities_and_conservation_through_cycles() {
    // Authorization §9 A/D/G: a full tour of all eight regions —
    // repeated deactivate/reactivate cycles — preserves every NPC
    // identity (exactly one chain obligation per NPC), conservation,
    // and drops nothing; every activation reapplies exactly one
    // captured episode.
    let scenario = PresenceScenario::Tour {
        ticks_per_region: 75,
    };
    let mut driver = region_driver(scenario);
    let centers = RegionConfig::default().centers;
    driver
        .run_headless_with_presence(600, |tick| vec![scenario.sample_at(tick, &centers)])
        .expect("presence run");
    let spine = driver.spine();
    assert_eq!(
        spine.pending_count() + spine.blocked_count() + spine.held_count(),
        NPC_COUNT,
        "exactly one obligation per NPC — no identity loss"
    );
    assert!(conservation(&driver));
    assert_eq!(spine.stats().invalid_dropped, 0, "nothing dropped");
    let diagnostics = driver.director().expect("director").diagnostics();
    assert!(diagnostics.deactivations >= 4, "real dormancy happened");
    assert!(diagnostics.activations >= 4, "real reactivation happened");
    assert_eq!(
        diagnostics.reapplied_deltas, diagnostics.activations,
        "each activation reapplied exactly one captured episode"
    );
    assert!(
        diagnostics.reconstructed_activities > 0,
        "reconstruction actually reconstructed"
    );
}

#[test]
fn reconstruction_labels_and_adaptive_selection() {
    // Authorization §9 E / reviewer priority A: short dormancy
    // reconstructs by exact replay; long dormancy chooses synthesis —
    // labeled, and the two produce different authoritative states.
    let centers = RegionConfig::default().centers;
    // Short dormancy: the presence walks from region 0 toward region 4
    // over 30 ticks — region 4 reactivates after a short held span
    // (below the adaptive threshold).
    let short_scenario = PresenceScenario::Walk {
        from: (100.0, 0.0),
        to: (-100.0, 0.0),
        ticks: 30,
    };
    let mut short = region_driver(short_scenario);
    short
        .run_headless_with_presence(40, |tick| vec![short_scenario.sample_at(tick, &centers)])
        .expect("short run");
    let short_state = &short.director().expect("director").states()[4];
    assert_eq!(
        short_state.last_method,
        Some(ReconstructionMethod::Replay),
        "short dormancy reconstructs by exact replay"
    );
    assert!(
        short_state.reconstructed_activities > 0,
        "the replay reconstructed activities"
    );
    // Long dormancy: the presence stays at region 0 for 200 ticks
    // before moving to region 4's center — the span exceeds the
    // adaptive threshold, so synthesis is chosen and labeled.
    let mut long = region_driver(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    long.run_headless(200);
    long.set_presence(vec![PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    }]);
    long.run_headless(10);
    let long_state = &long.director().expect("director").states()[4];
    assert_eq!(
        long_state.last_method,
        Some(ReconstructionMethod::Synthesis),
        "long dormancy chooses labeled synthesis"
    );
    assert!(
        long_state.reconstructed_activities > 0,
        "the synthesis reconstructed activities"
    );
    // Labels are authoritative: the two runs hash differently.
    assert_ne!(
        stage4_state_fingerprint_hex(short.spine(), short.director().as_ref().unwrap()),
        stage4_state_fingerprint_hex(long.spine(), long.director().as_ref().unwrap()),
        "different reconstruction methods produce different states"
    );
}

#[test]
fn capture_ledger_is_exactly_once() {
    // Authorization §9 D: captured changes survive discard and reapply
    // exactly once; duplicate attempts are no-ops; pending is never
    // falsely confirmed as applied.
    let mut ledger = CaptureLedger::default();
    assert!(ledger.capture(7, 42));
    assert!(!ledger.capture(7, 99), "duplicate capture is a no-op");
    assert_eq!(ledger.status(7), CaptureStatus::Pending(42));
    assert_eq!(
        ledger.apply(7),
        Some(42),
        "first application returns the delta"
    );
    assert_eq!(ledger.status(7), CaptureStatus::Applied);
    assert_eq!(ledger.apply(7), None, "duplicate application is a no-op");
    assert!(
        !ledger.capture(7, 100),
        "re-capturing an applied key is a no-op"
    );
    assert_eq!(ledger.pending_count(), 0);
    assert_eq!(ledger.applied_count(), 1);
    assert_eq!(ledger.status(999), CaptureStatus::Unknown);
}

#[test]
fn watermark_stays_safe_during_dormancy() {
    // Authorization §9 G: while dormant regions hold overdue
    // obligations, the safe watermark cannot claim completeness past
    // them — dormancy is not a completeness loophole.
    let mut driver = region_driver(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    driver.run_headless(300);
    let spine = driver.spine();
    assert!(spine.held_count() > 0);
    // The dormant regions' held dues (initial spread ≤ 13) cap the
    // watermark even though max_executed_due has grown large.
    assert!(
        spine
            .processed_through()
            .is_some_and(|tick| tick.as_u64() <= 13),
        "the watermark stays capped by held overdue work"
    );
    assert!(
        spine
            .max_executed_due()
            .is_some_and(|tick| tick.as_u64() > 100),
        "real work has executed far past the held dues"
    );
    assert!(conservation(&driver));
}

#[test]
fn region_runs_are_deterministic_across_double_runs() {
    // Authorization §9 F: identical seed + inputs + completed ticks →
    // identical authoritative Stage 4 fingerprints, for every scenario.
    let run = |scenario: PresenceScenario, ticks: u64| {
        let centers = RegionConfig::default().centers;
        let mut driver = region_driver(scenario);
        driver
            .run_headless_with_presence(ticks, |tick| vec![scenario.sample_at(tick, &centers)])
            .expect("presence run");
        stage4_state_fingerprint_hex(driver.spine(), driver.director().as_ref().unwrap())
    };
    for scenario in [
        PresenceScenario::Tour {
            ticks_per_region: 75,
        },
        PresenceScenario::Walk {
            from: (100.0, 0.0),
            to: (-100.0, 0.0),
            ticks: 300,
        },
        PresenceScenario::Stationary {
            position: (100.0, 0.0),
        },
    ] {
        assert_eq!(
            run(scenario, 300),
            run(scenario, 300),
            "double-run determinism for {scenario:?}"
        );
    }
}

#[test]
fn director_routes_new_dormant_targeted_obligations_to_held() {
    // Reviewer priority B: new obligations targeting a dormant region
    // enter the held state directly — they cannot execute unnoticed,
    // however far time advances.
    let mut driver = region_driver(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    let id = driver
        .schedule(
            EventSpec::new(
                EventClass::new(4),
                Schedule::AtLogicalTime { due: Tick(50) },
                0x8000_0000_0000_0001,
            )
            .in_field(Field::Far),
        )
        .expect("director schedule validates");
    assert_eq!(
        driver.spine().held_count(),
        5 * 125 + 1,
        "routed directly to held"
    );
    driver.run_headless(100);
    assert!(
        !driver.spine().is_executed(id),
        "the dormant-targeted obligation never executes"
    );
    assert!(conservation(&driver));
}

#[test]
fn activation_protects_promoted_regions_under_load() {
    // Reviewer priority C integration: a region reactivating from
    // Dormant lands in the NEAR lane immediately (resume then
    // reclassify) — its resumed obligations are eligible for the
    // reservation at once, with no starvation window.
    let mut driver = region_driver(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    driver.run_headless(200); // region 4 dormant throughout
    driver.set_presence(vec![PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    }]);
    let before = driver.spine().stats().executed;
    driver.run_headless(1);
    let spine = driver.spine();
    // The resumed obligations execute on the very first tick — they are
    // in the near lane with low (overdue) dues, first in priority.
    let resumed = spine.stats().executed - before;
    assert!(
        resumed > 50,
        "the reactivated region resumes immediately ({resumed})"
    );
    let region_4: Vec<Field> = spine
        .ledger()
        .iter()
        .filter(|record| record.class.0 == 4)
        .map(|record| record.field)
        .collect();
    assert!(!region_4.is_empty(), "region 4 executed after reactivation");
    assert!(
        region_4.iter().all(|field| *field == Field::Near),
        "resumed obligations execute in the near lane"
    );
    assert!(conservation(&driver));
}
