//! Stage 4 Batch 2 region contracts (authorization §9 A–H subset,
//! reviewer priorities A–C, Batch 2 corrective findings A–D):
//! presence-driven LOD across all levels, true dormancy (zero ordinary
//! work), identity and conservation through transition cycles, genuine
//! capture-before-discard with exactly-once consequence application,
//! coherent activation failure, represented-through-boundary
//! reconstruction under rapid transitions (finding D), reconstruction
//! labels + adaptive selection, per-tick scripted presence with
//! cross-cadence equivalence, watermark safety during dormancy,
//! immediate near-lane protection on reactivation, and determinism
//! across double runs.

use std::collections::HashSet;
use std::time::Duration;

use engine_core::{EventClass, EventSpec, Field, LodLevel, Schedule, Tick};
use engine_persist::ConsequenceId;
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

fn region_driver_with(config: SimConfig, presence: PresenceScenario) -> SimDriver {
    let region_config = RegionConfig::default();
    let initial = [presence.sample_at(0, &region_config.centers)];
    let mut driver = SimDriver::with_regions(NpcConfig::default(), config, region_config, &initial)
        .expect("valid region config");
    // Corrective finding B: scripted presence is per-tick — the driver
    // samples the scenario at each logical tick inside the tick loop.
    driver.set_presence_scenario(presence);
    driver
}

fn region_driver(presence: PresenceScenario) -> SimDriver {
    region_driver_with(SimConfig::default(), presence)
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

/// The structured identity the crossing's ledger speaks (the Batch 4
/// migration): the same (region, episode, npc) triples the legacy
/// `consequence_key` packed, as ConsequenceIds.
fn crossing_key(region: u32, episode: u64, npc: usize) -> ConsequenceId {
    ConsequenceId::new(region, episode, npc as u32)
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
    let key = ConsequenceId::new(0, 0, 7);
    let unknown = ConsequenceId::new(0, 0, 999);
    assert!(ledger.capture(key, 42));
    assert!(!ledger.capture(key, 99), "duplicate capture is a no-op");
    assert_eq!(ledger.status(key), CaptureStatus::Pending(42));
    assert_eq!(
        ledger.apply(key),
        Ok(ApplyOutcome::Applied(42)),
        "first application returns the delta"
    );
    assert_eq!(ledger.status(key), CaptureStatus::Applied);
    assert_eq!(
        ledger.apply(key),
        Ok(ApplyOutcome::AlreadyApplied),
        "duplicate application is an explicit no-op"
    );
    assert!(
        !ledger.capture(key, 100),
        "re-capturing an applied key is a no-op"
    );
    assert_eq!(ledger.pending_count(), 0);
    assert_eq!(ledger.applied_count(), 1);
    assert_eq!(ledger.status(unknown), CaptureStatus::Unknown);
    assert_eq!(
        ledger.apply(unknown),
        Err(ApplyError::Unknown),
        "an unknown key is an explicit application failure"
    );
    assert_eq!(ledger.pending_total(), 0, "nothing pending remains");
}

#[test]
fn structured_keys_are_the_stage4_packed_layout() {
    // The Batch 4 crossing's compatibility pin (the proposal §5.5: the
    // triple↔packed↔structured mappings proven by compatibility
    // tests): the structured ConsequenceId's packed Stage 4 form IS
    // this crate's legacy `consequence_key` expression — the bijection
    // the engine-persist boundary trio pinned against the mirrored
    // layout, proven here live against the authoritative source. The
    // ledger's fingerprint bytes therefore survive the structured-key
    // migration byte-identically (domain /2, packed keys, the
    // field-aligned ordering).
    let mut packed = HashSet::new();
    for region in [0u32, 1, 4, 7, 1022, 1023] {
        for npc in [0u32, 1, 999, 1000, 1022, 1023] {
            for episode in [0u64, 1, 2, 17, 1 << 43, (1 << 44) - 1] {
                let id = ConsequenceId::new(region, episode, npc);
                let key = id.pack_stage4().expect("in-domain");
                assert_eq!(key, consequence_key(region as usize, episode, npc as usize));
                assert_eq!(ConsequenceId::unpack_stage4(key), id);
                assert!(
                    packed.insert(key),
                    "the packing is injective over the sweep"
                );
            }
        }
    }
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
        let signal_keys: Vec<ConsequenceId> = checkpoint
            .entries
            .iter()
            .filter(|entry| entry.has_signal)
            .map(|entry| crossing_key(4, episode, entry.npc))
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
            .map(|entry| crossing_key(4, episode, entry.npc))
            .collect::<Vec<ConsequenceId>>()
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
        let signal_keys: Vec<ConsequenceId> = checkpoint
            .entries
            .iter()
            .filter(|entry| entry.has_signal)
            .map(|entry| crossing_key(4, episode, entry.npc))
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

#[test]
fn rapid_transitions_reconstruct_only_the_unrepresented_interval() {
    // Batch 2 corrective finding D: after an activation reconstructs a
    // dormant span, the world's represented state is AHEAD of the
    // still-held obligations' payloads — only real execution re-bases
    // a chain. When the region deactivates again before those
    // obligations execute (budget 30 < the 125 routine obligations, so
    // most stay pending), the checkpoint must capture the CURRENT
    // represented state and its represented-through boundary, and the
    // next activation must reconstruct ONLY the previously
    // unrepresented interval:
    //   - the first activation's representation is neither lost
    //     (cursor monotonicity) nor reconstructed a second time (the
    //     reconstructed-activity count is bounded by the short window);
    //   - positions stay consistent with the represented progression
    //     (at most one cell per activity);
    //   - the original outstanding obligations retain their identities
    //     and dues;
    //   - spine obligation conservation and consequential-state
    //     conservation hold.
    // The near-zero-elapsed double transition (one dormant tick)
    // contributes at most one legitimately-due activity per NPC —
    // never a re-representation of the dormant span. Both
    // reconstruction methods exercise the first activation.
    for (first_dormancy, expected_method) in [
        (200u64, ReconstructionMethod::Synthesis),
        (12u64, ReconstructionMethod::Replay),
    ] {
        let config = SimConfig {
            budget: 30,
            near_reserve: Some(10),
            ..SimConfig::default()
        };
        let at = |position: (f64, f64)| PresenceScenario::Stationary { position };
        let mut driver = region_driver_with(config, at((-100.0, 0.0)));
        driver.run_headless(150); // region 4 Full — chains advance
        // Dormant span 1: long enough that reactivation must
        // reconstruct (synthesis for 200 ticks, replay for 12).
        driver.set_presence_scenario(at((100.0, 0.0)));
        driver.run_headless(first_dormancy);
        // Snapshot the original outstanding obligations (identity,
        // due, npc) while held.
        let original: Vec<(engine_core::EventId, Tick, usize)> = driver
            .spine()
            .held_view()
            .iter()
            .filter(|view| view.class.0 == 4)
            .filter_map(|view| {
                routine_payload_parts(view.payload).map(|(npc, _)| (view.id, view.due, npc))
            })
            .collect();
        assert_eq!(original.len(), 125, "the region's routine chains are held");
        // Reactivation: reconstruction advances the represented state
        // past most payloads; the budget-starved tick leaves most
        // resumed obligations pending (at most ~30 of 125 execute).
        driver.set_presence_scenario(at((-100.0, 0.0)));
        driver.run_headless(1);
        assert_eq!(
            driver.director().expect("director").states()[4].last_method,
            Some(expected_method),
            "the first dormancy exercises {expected_method:?}"
        );
        // Snapshot the represented state after the first activation's
        // execution tick.
        let (cursor_before, position_before): (Vec<u64>, Vec<(u8, u8)>) = {
            let world = driver.spine().world();
            (
                region_npcs(4)
                    .map(|npc| world.routine_cursor()[npc])
                    .collect(),
                region_npcs(4).map(|npc| world.positions()[npc]).collect(),
            )
        };
        // Deactivate again — one boundary later, before the remaining
        // obligations execute.
        driver.set_presence_scenario(at((100.0, 0.0)));
        driver.run_headless(1);
        // The never-executed originals are held again — with their
        // ORIGINAL identities and dues.
        {
            let spine = driver.spine();
            let held_pairs: HashSet<(u64, u64)> = spine
                .held_view()
                .iter()
                .filter(|view| view.class.0 == 4)
                .map(|view| (view.id.seq, view.due.as_u64()))
                .collect();
            let mut survivors = 0;
            for (id, due, _) in &original {
                if !spine.is_executed(*id) {
                    survivors += 1;
                    assert!(
                        held_pairs.contains(&(id.seq, due.as_u64())),
                        "never-executed obligation {} retains identity and due",
                        id.seq
                    );
                }
            }
            assert!(
                survivors >= 60,
                "the budget-starved window leaves most obligations pending ({survivors})"
            );
        }
        // Reactivate after a near-zero interval (one dormant tick):
        // only the unrepresented interval is reconstructed.
        driver.set_presence_scenario(at((-100.0, 0.0)));
        driver.run_headless(1);
        {
            let director = driver.director().expect("director");
            let state = &director.states()[4];
            assert!(
                state.reconstructed_activities <= NPC_COUNT as u64,
                "at most one window activity per NPC is reconstructed ({})",
                state.reconstructed_activities
            );
            assert_eq!(
                state.last_method,
                Some(ReconstructionMethod::Replay),
                "the unrepresented interval is small — the estimate sees only it"
            );
        }
        // Per-NPC progression bounds: at most one reconstructed window
        // activity plus at most one real execution across the two
        // activation ticks — monotone, and positions move consistently
        // with the represented progression.
        {
            let spine = driver.spine();
            let world = spine.world();
            for (index, npc) in region_npcs(4).enumerate() {
                let cursor_delta = world.routine_cursor()[npc] - cursor_before[index];
                assert!(
                    cursor_delta <= 2,
                    "npc {npc}: at most one window activity + one execution ({cursor_delta})"
                );
                let (dx, dy) = (
                    i16::from(world.positions()[npc].0) - i16::from(position_before[index].0),
                    i16::from(world.positions()[npc].1) - i16::from(position_before[index].1),
                );
                assert!(
                    u64::from(dx.unsigned_abs() + dy.unsigned_abs()) <= cursor_delta,
                    "npc {npc}: position moves consistently with represented progression"
                );
                assert!(
                    world.positions()[npc].0 < REGION_GRID
                        && world.positions()[npc].1 < REGION_GRID,
                    "npc {npc} stays in bounds"
                );
            }
        }
        // Conservation: obligations and consequential state.
        assert!(conservation(&driver));
        {
            let spine = driver.spine();
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
                "consequence conservation through the rapid transitions"
            );
        }
    }
}

#[test]
fn malformed_routine_events_cannot_corrupt_a_foreign_region() {
    // Batch 3 defensive check 1: a routine-TAGGED event whose NPC
    // identity belongs to a different region than its event class is
    // ignored by the workload and never joins the class-region's
    // checkpoint — it cannot advance another region's NPC state or
    // corrupt its reconstruction data. The spine still counts the
    // execution in its ledger.
    let at = |position: (f64, f64)| PresenceScenario::Stationary { position };
    let mut driver = region_driver(at((-100.0, 0.0)));
    driver.run_headless(150); // region 4 Full — chains advance
    // Activate region 2 (its center) and let its chains run.
    driver.set_presence_scenario(at((0.0, 100.0)));
    driver.run_headless(50);
    // A routine-tagged event for npc 5 (home region 5) scheduled with
    // CLASS 2 while region 2 is active — the malformed combination.
    let malformed = driver
        .schedule(
            EventSpec::new(
                EventClass::new(2),
                Schedule::AtLogicalTime { due: Tick(205) },
                engine_slice::npc::routine_payload(5, 999),
            )
            .in_field(Field::Near),
        )
        .expect("director schedule validates");
    // The due is still in the future: the malformed event stays queued
    // while the region deactivates two ticks later.
    driver.run_headless(2);
    driver.set_presence_scenario(at((-100.0, 0.0)));
    driver.run_headless(30); // region 2 dormant — the malformed event is held
    {
        let director = driver.director().expect("director");
        let checkpoint = director.states()[2]
            .checkpoint
            .as_ref()
            .expect("region 2 dormant");
        assert_eq!(
            checkpoint.entries.len(),
            125,
            "only the region's own chains join the checkpoint"
        );
        assert!(
            checkpoint
                .entries
                .iter()
                .all(|entry| entry.npc % REGION_COUNT == 2),
            "no foreign-region NPC in region 2's checkpoint"
        );
        assert!(
            !checkpoint.entries.iter().any(|entry| entry.npc == 5),
            "the malformed event's NPC never joins the checkpoint"
        );
    }
    // Reactivate region 2: the malformed event resumes and executes
    // (the spine counts it), but the workload ignores it — npc 5's
    // state is untouched (its own region is dormant at this presence,
    // so any change could only come from the malformed event).
    let steps_before = driver.spine().world().steps()[5];
    driver.set_presence_scenario(at((0.0, 100.0)));
    driver.run_headless(50);
    let spine = driver.spine();
    assert!(
        spine.is_executed(malformed),
        "the malformed event executed (spine-ledger accounted)"
    );
    assert_eq!(
        spine.world().steps()[5],
        steps_before,
        "the malformed routine-tagged event never advanced npc 5's state"
    );
    assert!(conservation(&driver));
}

#[test]
fn activation_without_a_recovery_checkpoint_fails_closed() {
    // Batch 3 defensive check 2: a dormant region whose recovery
    // checkpoint is missing cannot have its suspended state
    // reconstructed — activation fails CLOSED: the region stays
    // dormant, its obligations stay held, and the anomaly is reported
    // on every promotion attempt. Never a silent promotion of
    // unreconstructible state.
    let mut driver = region_driver(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    driver.run_headless(150);
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (100.0, 0.0),
    });
    driver.run_headless(100); // region 4 dormant with a checkpoint
    let held_region_4 = driver
        .spine()
        .held_view()
        .iter()
        .filter(|view| view.class.0 == 4)
        .count();
    let invalid_before = driver
        .director()
        .expect("director")
        .diagnostics()
        .invalid_transitions;
    // Corrupt the recovery state (test-only seam): without a
    // checkpoint the suspended state cannot be reconstructed.
    driver
        .director_mut()
        .expect("director")
        .debug_clear_checkpoint(4);
    driver.set_presence_scenario(PresenceScenario::Stationary {
        position: (-100.0, 0.0),
    });
    driver.run_headless(10); // a promotion attempt every tick boundary
    {
        let spine = driver.spine();
        let director = driver.director().expect("director");
        assert_eq!(
            director.levels()[4],
            LodLevel::Dormant,
            "the region is never promoted without a recovery checkpoint"
        );
        assert_eq!(
            spine
                .held_view()
                .iter()
                .filter(|view| view.class.0 == 4)
                .count(),
            held_region_4,
            "the region's obligations stay held"
        );
        assert!(
            director.diagnostics().invalid_transitions > invalid_before,
            "the anomaly is reported on every promotion attempt"
        );
    }
    assert!(conservation(&driver));
}

#[test]
fn per_lod_work_scales_across_levels() {
    // Stage 4 final-verification efficiency evidence: one stable
    // presence position producing all four LOD levels simultaneously —
    // region 0 Full (≈21 units), region 1 Simplified (≈59), regions 2+7
    // EventDriven, regions 3–6 Dormant. Lower levels do meaningfully
    // less work at the same logical tick count; dormant regions do
    // none. Printed (via --nocapture) for the delivery report:
    // per-class execution counts, tick-time percentiles, obligation
    // counts, and the total against the all-active Legacy baseline.
    let mut driver = region_driver_with(
        SimConfig::default(),
        PresenceScenario::Stationary {
            position: (95.0, 20.0),
        },
    );
    driver.run_headless(300);
    let spine = driver.spine();
    let levels = driver.director().expect("director").levels();
    assert_eq!(levels[0], LodLevel::Full, "region 0 at ≈21 units");
    assert_eq!(levels[1], LodLevel::Simplified, "region 1 at ≈59 units");
    assert_eq!(levels[2], LodLevel::EventDriven, "region 2 at ≈124 units");
    assert_eq!(levels[7], LodLevel::EventDriven, "region 7 at ≈97 units");
    for region in [3, 4, 5, 6] {
        assert_eq!(levels[region], LodLevel::Dormant);
    }
    let executed_in = |class: u16| {
        spine
            .ledger()
            .iter()
            .filter(|record| record.class.0 == class)
            .count()
    };
    let full = executed_in(0);
    let simplified = executed_in(1);
    let event = executed_in(2);
    let event_7 = executed_in(7);
    let dormant: usize = (3..=6).map(executed_in).sum();
    println!(
        "per-LOD over 300 ticks: full {full} · simplified {simplified} · \
         event(2) {event} · event(7) {event_7} · dormant(3-6) {dormant}"
    );
    println!(
        "spine tick time (per-tick): p50 {:.3} ms · p95 {:.3} ms",
        driver.tick_ms_percentile(0.50),
        driver.tick_ms_percentile(0.95)
    );
    println!(
        "region director boundary (per-tick, presence sampling + transitions): \
         p50 {:.3} ms · p95 {:.3} ms",
        driver.director_ms_percentile(0.50),
        driver.director_ms_percentile(0.95)
    );
    println!(
        "obligations: queued {} · blocked {} · held {}",
        spine.pending_count(),
        spine.blocked_count(),
        spine.held_count()
    );
    assert!(
        full > 4_000,
        "Full executes at the natural cadence ({full})"
    );
    assert!(
        simplified * 3 < full,
        "Simplified executes meaningfully less ({simplified} vs {full})"
    );
    assert!(
        event * 2 < simplified,
        "EventDriven executes meaningfully less ({event} vs {simplified})"
    );
    assert_eq!(
        dormant, 0,
        "dormant regions execute nothing — zero ordinary work"
    );
    // The measurable dormancy reduction against the all-active Legacy
    // baseline (45,717 executions over the same 300 ticks) — fewer
    // executions is the measured fact here; total CPU-time reduction is
    // reported separately, not inferred from this count alone.
    let total = spine.stats().executed;
    println!("total executions {total} (all-active Legacy baseline: 45,717)");
    assert!(
        total < 20_000,
        "proximity-driven LOD measurably reduces total work ({total})"
    );
    assert!(conservation(&driver));
}

#[test]
fn activation_latency_stays_under_the_delivery_budget() {
    // Stage 4 acceptance requirement: a 125-NPC region activation stays
    // under 50 ms. The authoritative measurement is the Windows rig
    // (the README Stage 4 acceptance commands); this pins the budget on
    // every platform the suite runs on, using the full tour's
    // activation-latency distribution (observational percentiles from
    // the director).
    let mut driver = region_driver(PresenceScenario::Tour {
        ticks_per_region: 75,
    });
    driver.run_headless(600);
    let director = driver.director().expect("director");
    let diagnostics = director.diagnostics();
    assert!(
        diagnostics.activations >= 4,
        "the tour performed real activations ({})",
        diagnostics.activations
    );
    let p50 = director.activation_latency_percentile(0.50);
    let p95 = director.activation_latency_percentile(0.95);
    let worst = director.activation_latency_percentile(1.0);
    println!(
        "activation latency over {} activations: p50 {p50:.3} ms · \
         p95 {p95:.3} ms · worst {worst:.3} ms",
        diagnostics.activations
    );
    assert!(
        p95 < 50.0,
        "125-NPC region activation p95 stays under the 50 ms budget ({p95:.3} ms)"
    );
    assert!(
        worst < 50.0,
        "125-NPC region activation worst stays under the 50 ms budget ({worst:.3} ms)"
    );
}
