//! Stage 4 Batch 2 region contracts (authorization §9 A–H subset,
//! reviewer priorities A–C, Batch 2 corrective findings A–C):
//! presence-driven LOD across all levels, true dormancy (zero ordinary
//! work), identity and conservation through transition cycles, genuine
//! capture-before-discard with exactly-once consequence application,
//! coherent activation failure, reconstruction labels + adaptive
//! selection, per-tick scripted presence with cross-cadence
//! equivalence, watermark safety during dormancy, immediate near-lane
//! protection on reactivation, and determinism across double runs.

use std::time::Duration;

use engine_core::{EventClass, EventSpec, Field, LodLevel, Schedule, Tick};
use engine_slice::npc::{
    NPC_COUNT, NpcConfig, POSITION_DISCARDED, REGION_COUNT, REGION_GRID, ROUTINE_CURSOR_DISCARDED,
    routine_payload_parts,
};
use engine_slice::region::{
    ApplyError, ApplyOutcome, CaptureLedger, CaptureStatus, PresenceScenario, ReconstructionMethod,
    RegionConfig, consequence_key, stage4_state_fingerprint_hex,
};
use engine_slice::sim::{SimConfig, SimDriver};
use engine_slice::sim_demo::irregular_frame;

fn region_driver(presence: PresenceScenario) -> SimDriver {
    let region_config = RegionConfig::default();
    let initial = [presence.sample_at(0, &region_config.centers)];
    let mut driver = SimDriver::with_regions(
        NpcConfig::default(),
        SimConfig::default(),
        region_config,
        &initial,
    )
    .expect("valid region config");
    // Corrective finding B: scripted presence is per-tick — the driver
    // samples the scenario at each logical tick inside the tick loop.
    driver.set_presence_scenario(presence);
    driver
}

/// The stable NPC identities belonging to one region (region = id mod 8).
fn region_npcs(region: usize) -> impl Iterator<Item = usize> {
    (0..NPC_COUNT).filter(move |npc| npc % REGION_COUNT == region)
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

/// Advances a driver under a synthetic frame cadence until exactly
/// `target` campaign ticks (the demo's comparability rule: every frame
/// is capped so it cannot accrue more due ticks than remain — every
/// cadence reports at exactly the same completed tick count). A
/// cadence of 0 selects the irregular pattern including the
/// beyond-catch-up-bound stall.
fn run_frames_until(driver: &mut SimDriver, target: u64, cadence_hz: u32) {
    let period_ns = 1_000_000_000u64 / u64::from(driver.config().tick_rate_hz.max(1));
    let mut frame_index = 0u64;
    loop {
        let clock = driver.spine().clock().as_u64();
        if clock >= target {
            break;
        }
        let outstanding = driver.outstanding_ticks();
        let remaining = (target - clock) as u32;
        if outstanding >= remaining {
            driver.run_headless(u64::from(remaining));
            break;
        }
        let frame = match cadence_hz {
            0 => irregular_frame(frame_index),
            hz => Duration::from_secs_f64(1.0 / f64::from(hz.max(1))),
        };
        let cap = Duration::from_nanos(u64::from(remaining - outstanding) * period_ns);
        driver.advance(frame.min(cap));
        frame_index += 1;
    }
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
    // Steps frozen for the dormant region's NPCs, and the
    // reconstructible representation DISCARDED (corrective finding A):
    // sentinels, not flags — the representation is gone until
    // reconstruction restores it from the checkpoint.
    for npc in region_npcs(4).take(5) {
        let world = driver.spine().world();
        assert_eq!(world.steps()[npc], 0);
        assert_eq!(
            world.routine_cursor()[npc],
            ROUTINE_CURSOR_DISCARDED,
            "the routine cursor was discarded at dormancy"
        );
        assert_eq!(
            world.positions()[npc],
            POSITION_DISCARDED,
            "the grid position was discarded at dormancy"
        );
        assert_eq!(
            world.pending_signals()[npc],
            0,
            "no uncommitted signal scratch remains (nothing was ever active)"
        );
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
    // and drops nothing; every captured consequence is applied at most
    // once, and consequence conservation holds end to end.
    let scenario = PresenceScenario::Tour {
        ticks_per_region: 75,
    };
    let mut driver = region_driver(scenario);
    driver.run_headless(600);
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
    assert!(
        diagnostics.reapplied_deltas <= diagnostics.captured_deltas,
        "every committed consequence corresponds to one capture"
    );
    assert!(
        diagnostics.reconstructed_activities > 0,
        "reconstruction actually reconstructed"
    );
    // Consequence conservation (corrective finding A): every real
    // milestone crossing is exactly one of committed credit,
    // uncommitted scratch, or captured-unapplied ledger delta —
    // nothing consequential is lost through the cycles.
    let world = spine.world();
    let milestones: u64 = world.milestones().iter().sum();
    let credits: u64 = world.credits().iter().sum();
    let pending: u64 = world.pending_signals().iter().sum();
    let captured = driver
        .director()
        .expect("director")
        .ledger()
        .pending_total();
    assert_eq!(
        milestones,
        credits + pending + captured,
        "consequence conservation through deactivate/reactivate cycles"
    );
}

#[test]
fn reconstruction_labels_and_adaptive_selection() {
    // Authorization §9 E / reviewer priority A: short dormancy
    // reconstructs by exact replay; long dormancy chooses synthesis —
    // labeled, and the two produce different authoritative states.
    // Short dormancy: the presence walks from region 0 toward region 4
    // over 30 ticks — region 4 reactivates after a short held span
    // (below the adaptive threshold).
    let short_scenario = PresenceScenario::Walk {
        from: (100.0, 0.0),
        to: (-100.0, 0.0),
        ticks: 30,
    };
    let mut short = region_driver(short_scenario);
    short.run_headless(40);
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
    long.set_presence_scenario(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
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
    // Authorization §9 D / Batch 2 corrective findings A and C:
    // captured changes survive discard and reapply exactly once;
    // duplicate attempts are no-ops with an explicit outcome; an
    // unknown key is an explicit failure (the caller must fail
    // coherently, never silently promote); pending is never falsely
    // confirmed as applied.
    let mut ledger = CaptureLedger::default();
    assert!(ledger.capture(7, 42));
    assert!(!ledger.capture(7, 99), "duplicate capture is a no-op");
    assert_eq!(ledger.status(7), CaptureStatus::Pending(42));
    assert_eq!(
        ledger.apply(7),
        Ok(ApplyOutcome::Applied(42)),
        "first application returns the delta"
    );
    assert_eq!(ledger.status(7), CaptureStatus::Applied);
    assert_eq!(
        ledger.apply(7),
        Ok(ApplyOutcome::AlreadyApplied),
        "duplicate application is an explicit no-op"
    );
    assert!(
        !ledger.capture(7, 100),
        "re-capturing an applied key is a no-op"
    );
    assert_eq!(ledger.pending_count(), 0);
    assert_eq!(ledger.applied_count(), 1);
    assert_eq!(ledger.status(999), CaptureStatus::Unknown);
    assert_eq!(
        ledger.apply(999),
        Err(ApplyError::Unknown),
        "an unknown key is an explicit application failure"
    );
    assert_eq!(ledger.pending_total(), 0, "nothing pending remains");
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
        let mut driver = region_driver(scenario);
        driver.run_headless(ticks);
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
    // Scenario (per-tick) presence, not a frame sample — the scripted
    // path is what headless promotion uses (corrective finding B).
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
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

#[test]
fn capture_before_discard_survives_repeated_cycles() {
    // Batch 2 corrective finding A: a genuine consequential NPC state
    // change — the pending signal accrued by real milestone crossings
    // — is captured into the in-memory ledger BEFORE the
    // reconstructible representation is discarded, survives dormancy
    // as explicit pending state, and is committed exactly once at
    // reactivation across repeated deactivate/reactivate cycles.
    // Neither replay nor synthesis ever touches it.
    let mut driver = region_driver(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    driver.run_headless(150); // region 4 Full — real executions accrue
    let milestones_at_deactivation: u64 = {
        let world = driver.spine().world();
        let milestones: u64 = region_npcs(4).map(|npc| world.milestones()[npc]).sum();
        let pending: u64 = region_npcs(4).map(|npc| world.pending_signals()[npc]).sum();
        let credits: u64 = region_npcs(4).map(|npc| world.credits()[npc]).sum();
        assert!(milestones > 0, "real executions crossed milestones");
        assert_eq!(
            milestones, pending,
            "every crossing is uncommitted scratch while the region is active"
        );
        assert_eq!(credits, 0, "nothing commits before a transition");
        milestones
    };
    // Deactivate: presence moves to region 0 — region 4 goes dormant.
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    driver.run_headless(100);
    let (captured, signal_keys) = {
        let spine = driver.spine();
        let world = spine.world();
        // The reconstructible representation was DISCARDED — sentinels,
        // not flags.
        for npc in region_npcs(4).take(5) {
            assert_eq!(world.routine_cursor()[npc], ROUTINE_CURSOR_DISCARDED);
            assert_eq!(world.positions()[npc], POSITION_DISCARDED);
            assert_eq!(
                world.pending_signals()[npc],
                0,
                "signal scratch cleared after capture"
            );
        }
        // Authoritative consequential state is retained.
        let steps: u64 = region_npcs(4).map(|npc| world.steps()[npc]).sum();
        assert!(steps > 0, "steps retained through dormancy");
        let director = driver.director().expect("director");
        let state = &director.states()[4];
        assert_eq!(state.level, LodLevel::Dormant);
        let checkpoint = state.checkpoint.as_ref().expect("checkpoint retained");
        assert_eq!(checkpoint.entries.len(), 125);
        let episode = state.episode;
        let signal_keys: Vec<u64> = checkpoint
            .entries
            .iter()
            .filter(|entry| entry.has_signal)
            .map(|entry| consequence_key(4, episode, entry.npc))
            .collect();
        assert!(!signal_keys.is_empty(), "real consequences were captured");
        let pending_total = director.ledger().pending_total();
        assert_eq!(
            pending_total, milestones_at_deactivation,
            "the ledger holds exactly the crossings captured at deactivation"
        );
        // Explicit pending status — never falsely confirmed as applied.
        for key in &signal_keys {
            assert!(matches!(
                director.ledger().status(*key),
                CaptureStatus::Pending(_)
            ));
        }
        (pending_total, signal_keys)
    };
    // Reactivation commits each captured consequence exactly once and
    // reconstructs the discarded representation from the checkpoint.
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    driver.run_headless(1);
    {
        let spine = driver.spine();
        let world = spine.world();
        let credits: u64 = region_npcs(4).map(|npc| world.credits()[npc]).sum();
        assert_eq!(
            credits, captured,
            "the captured consequences committed exactly once"
        );
        let director = driver.director().expect("director");
        for key in &signal_keys {
            assert_eq!(
                director.ledger().status(*key),
                CaptureStatus::Applied,
                "episode keys are applied — never re-capturable"
            );
        }
        for npc in region_npcs(4).take(5) {
            assert_ne!(
                world.routine_cursor()[npc],
                ROUTINE_CURSOR_DISCARDED,
                "reconstruction restored the cursor"
            );
            let (x, y) = world.positions()[npc];
            assert!(x < REGION_GRID && y < REGION_GRID, "restored in bounds");
        }
        // Crossings since the activation are scratch again.
        let milestones: u64 = region_npcs(4).map(|npc| world.milestones()[npc]).sum();
        let pending: u64 = region_npcs(4).map(|npc| world.pending_signals()[npc]).sum();
        assert_eq!(
            milestones,
            credits + pending,
            "region conservation: crossings == credits + scratch"
        );
    }
    // Accrue more consequences, then cycle again — a fresh episode
    // with fresh keys; the old keys can never double-commit.
    driver.run_headless(50);
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    driver.run_headless(100);
    let episode2_keys = {
        let director = driver.director().expect("director");
        let state = &director.states()[4];
        assert_eq!(state.level, LodLevel::Dormant, "second dormancy");
        let checkpoint = state.checkpoint.as_ref().expect("second checkpoint");
        let episode = state.episode;
        checkpoint
            .entries
            .iter()
            .filter(|entry| entry.has_signal)
            .map(|entry| consequence_key(4, episode, entry.npc))
            .collect::<Vec<u64>>()
    };
    assert!(
        !episode2_keys.is_empty(),
        "the second episode captured fresh consequences"
    );
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    driver.run_headless(1);
    {
        let spine = driver.spine();
        let director = driver.director().expect("director");
        for key in &episode2_keys {
            assert_eq!(director.ledger().status(*key), CaptureStatus::Applied);
        }
        for key in &signal_keys {
            assert_eq!(
                director.ledger().status(*key),
                CaptureStatus::Applied,
                "episode-1 keys stay applied — no double commit across cycles"
            );
        }
        let world = spine.world();
        let credits: u64 = region_npcs(4).map(|npc| world.credits()[npc]).sum();
        let milestones: u64 = region_npcs(4).map(|npc| world.milestones()[npc]).sum();
        let pending: u64 = region_npcs(4).map(|npc| world.pending_signals()[npc]).sum();
        assert_eq!(
            milestones,
            credits + pending,
            "region conservation holds across both cycles"
        );
    }
    assert!(conservation(&driver));
}

#[test]
fn activation_failure_preserves_a_coherent_dormant_state() {
    // Batch 2 corrective finding C: when an activation cannot apply its
    // captured consequences, the region is left SAFELY dormant —
    // checkpoint retained, obligations still held, captures still
    // pending, nothing committed or reconstructed — and the subsequent
    // attempt commits exactly once (no doubling from the failed
    // attempt).
    let mut driver = region_driver(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    driver.run_headless(150);
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    driver.run_headless(100); // region 4 dormant with real captures
    let (expected_credits, signal_keys, held_region_4, invalid_before) = {
        let spine = driver.spine();
        let director = driver.director().expect("director");
        let state = &director.states()[4];
        assert_eq!(state.level, LodLevel::Dormant);
        let checkpoint = state.checkpoint.as_ref().expect("checkpoint");
        let episode = state.episode;
        let signal_keys: Vec<u64> = checkpoint
            .entries
            .iter()
            .filter(|entry| entry.has_signal)
            .map(|entry| consequence_key(4, episode, entry.npc))
            .collect();
        assert!(!signal_keys.is_empty(), "real captures exist to apply");
        let expected: u64 = signal_keys
            .iter()
            .map(|key| match director.ledger().status(*key) {
                CaptureStatus::Pending(delta) => delta,
                _ => panic!("freshly captured keys are pending"),
            })
            .sum();
        let held_region_4 = spine
            .held_view()
            .iter()
            .filter(|view| view.class.0 == 4)
            .count();
        (
            expected,
            signal_keys,
            held_region_4,
            director.diagnostics().invalid_transitions,
        )
    };
    // Inject a region-targeted application failure; the promotion
    // attempt must fail coherently — no partial promotion, no consumed
    // checkpoint. (Region-specific: regions 3 and 5 legitimately
    // activate at the same tick boundary and must be unaffected.)
    driver
        .director_mut()
        .expect("director")
        .debug_inject_apply_failure(4);
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    driver.run_headless(1);
    {
        let spine = driver.spine();
        let director = driver.director().expect("director");
        assert_eq!(
            director.levels()[4],
            LodLevel::Dormant,
            "the region is not half-promoted"
        );
        assert!(
            director.states()[4].checkpoint.is_some(),
            "the recovery checkpoint is intact"
        );
        assert_eq!(
            spine
                .held_view()
                .iter()
                .filter(|view| view.class.0 == 4)
                .count(),
            held_region_4,
            "the region's obligations are still held"
        );
        for key in &signal_keys {
            assert!(
                matches!(director.ledger().status(*key), CaptureStatus::Pending(_)),
                "the captures are still pending"
            );
        }
        let credits: u64 = region_npcs(4).map(|npc| spine.world().credits()[npc]).sum();
        assert_eq!(credits, 0, "nothing committed by the failed attempt");
        for npc in region_npcs(4).take(5) {
            assert_eq!(
                spine.world().routine_cursor()[npc],
                ROUTINE_CURSOR_DISCARDED,
                "the discarded representation was not reconstructed"
            );
        }
        assert_eq!(
            director.diagnostics().invalid_transitions,
            invalid_before + 1,
            "the failure is reported, never silent"
        );
    }
    // The next tick retries and succeeds — committing exactly once.
    driver.run_headless(1);
    {
        let spine = driver.spine();
        let director = driver.director().expect("director");
        assert_eq!(director.levels()[4], LodLevel::Full, "the retry promotes");
        assert!(director.states()[4].checkpoint.is_none());
        let credits: u64 = region_npcs(4).map(|npc| spine.world().credits()[npc]).sum();
        assert_eq!(
            credits, expected_credits,
            "the consequences committed exactly once despite the failed attempt"
        );
        for key in &signal_keys {
            assert_eq!(director.ledger().status(*key), CaptureStatus::Applied);
        }
        assert_eq!(
            director.diagnostics().invalid_transitions,
            invalid_before + 1,
            "the successful retry reports no further anomaly"
        );
    }
    assert!(conservation(&driver));
}

#[test]
fn scripted_presence_is_equivalent_across_frame_cadences() {
    // Batch 2 corrective finding B: with per-tick scripted presence,
    // the authoritative Stage 4 state is a function of (seed, config,
    // presence path, completed logical ticks) — NOT of the frame
    // cadence delivering the ticks. The headless reference, 60 Hz,
    // 240 Hz, and the irregular pattern (with its
    // beyond-catch-up-bound stall and retained-obligation drain) all
    // complete the same tick count and hash identically. Camera
    // presence is deliberately not claimed here: live input is
    // frame-sampled and genuinely cadence-dependent.
    let scenario = PresenceScenario::Tour {
        ticks_per_region: 75,
    };
    let fingerprint = |mode: Option<u32>| {
        let mut driver = region_driver(scenario);
        match mode {
            None => driver.run_headless(300),
            Some(hz) => run_frames_until(&mut driver, 300, hz),
        }
        stage4_state_fingerprint_hex(driver.spine(), driver.director().as_ref().unwrap())
    };
    let reference = fingerprint(None);
    assert_eq!(fingerprint(Some(60)), reference, "60 Hz frame cadence");
    assert_eq!(fingerprint(Some(240)), reference, "240 Hz frame cadence");
    assert_eq!(
        fingerprint(Some(0)),
        reference,
        "irregular cadence with the catch-up stall"
    );
}

#[test]
fn post_checkpoint_dormant_obligations_resume_without_routine_confusion() {
    // Reviewer addition: an obligation scheduled into an
    // already-dormant region AFTER the checkpoint was created resumes
    // with its original identity on reactivation (executing late) —
    // and is never mistaken for a routine NPC chain: only
    // explicitly-typed routine payloads reconstruct or advance NPC
    // state. The one-obligation-per-NPC invariant is scoped to the
    // routine-chain workload; the foreign event is accounted
    // separately.
    let mut driver = region_driver(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    driver.run_headless(150);
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    driver.run_headless(100); // region 4 dormant, checkpoint written
    {
        let director = driver.director().expect("director");
        let checkpoint = director.states()[4]
            .checkpoint
            .as_ref()
            .expect("checkpoint");
        assert_eq!(
            checkpoint.entries.len(),
            125,
            "the checkpoint covers the region's routine chains"
        );
    }
    // A foreign obligation targeting the dormant region, scheduled
    // AFTER the checkpoint was created. Its payload (4) would decode
    // as npc 0 under the Legacy layout — but it carries no routine
    // type tag, so it is not a routine chain.
    let foreign_id = driver
        .schedule(
            EventSpec::new(
                EventClass::new(4),
                Schedule::AtLogicalTime { due: Tick(50) },
                4,
            )
            .in_field(Field::Far),
        )
        .expect("director schedule validates");
    {
        let spine = driver.spine();
        assert_eq!(spine.held_count(), 5 * 125 + 1, "routed directly to held");
        let routine_held = spine
            .held_view()
            .iter()
            .filter(|view| routine_payload_parts(view.payload).is_some())
            .count();
        assert_eq!(
            routine_held,
            5 * 125,
            "the foreign payload is not counted among routine chains"
        );
        let director = driver.director().expect("director");
        assert_eq!(
            director.states()[4]
                .checkpoint
                .as_ref()
                .expect("checkpoint")
                .entries
                .len(),
            125,
            "the foreign event never joins the checkpoint"
        );
    }
    // Reactivation: the foreign obligation resumes with its original
    // identity and executes late; the routine chains reconstruct. The
    // untyped payload never advances any NPC (explicit typing).
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    let steps_before = driver.spine().world().steps()[0];
    let late_before = driver.spine().stats().late_executions;
    driver.run_headless(50);
    let spine = driver.spine();
    assert!(
        spine.is_executed(foreign_id),
        "the post-checkpoint obligation resumed and executed"
    );
    assert!(
        spine.stats().late_executions > late_before,
        "it executed late — the original due was retained through dormancy"
    );
    assert_eq!(
        spine.world().steps()[0],
        steps_before,
        "the untyped payload never advanced npc 0 — explicit typing"
    );
    for npc in region_npcs(4).take(5) {
        assert_ne!(
            spine.world().routine_cursor()[npc],
            ROUTINE_CURSOR_DISCARDED,
            "the routine chains reconstructed"
        );
        let (x, y) = spine.world().positions()[npc];
        assert!(x < REGION_GRID && y < REGION_GRID, "in bounds");
    }
    // The one-per-NPC invariant is scoped to routine chains: the
    // foreign event has executed and left the queues; the 1000
    // routine chains remain exactly one obligation per NPC.
    assert_eq!(
        spine.pending_count() + spine.blocked_count() + spine.held_count(),
        NPC_COUNT,
        "routine-chain invariant after the foreign event executed"
    );
    assert!(conservation(&driver));
}
