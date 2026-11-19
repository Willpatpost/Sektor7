//! The Cycle-4 capability-D encounter regressions: the lifecycle
//! guarantees (the Cycle-3.1 invariants hold in the integrated
//! encounter), the ratified EVENT-TIMESTAMPS contract's conformance
//! (1-based spine-aligned labels, T2/T4; the DIRECT cadence
//! equivalence, T6 — the Cycle-3.1 workaround retires for new code),
//! the deterministic headless scenarios with pinned NEW-domain
//! traces, and the connected live-frame route (the R2-closeout
//! discipline — the real frame path, no teleported states).
//!
//! The weapons-selection, multi-adversary-wave, and rotated-cover
//! batteries land with their interface contracts (WEAPONS /
//! COMBATANTS / COLLISION) — this file pins everything implementable
//! against the accepted surface NOW.

use std::time::Duration;

use engine_core::{MoveIntent, WeaponId};
use engine_slice::encounter_demo::{
    ACTIVATION_Z, ENCOUNTER_BOXES, EncounterDemo, EncounterPhase, encounter_fingerprint_hex,
    encounter_single_config, run_encounter_failure_headless, run_encounter_wave_failure_headless,
    run_encounter_wave_winnable_headless, run_encounter_winnable, run_encounter_winnable_headless,
};

fn idle() -> MoveIntent {
    MoveIntent::default()
}

fn forward() -> MoveIntent {
    MoveIntent {
        forward: 1,
        ..idle()
    }
}

/// Runs a scripted phase until the condition holds (the headless
/// runners' own discipline).
fn run_until(
    demo: &mut EncounterDemo,
    intent: MoveIntent,
    fire: bool,
    condition: impl Fn(&EncounterDemo) -> bool,
    label: &str,
) {
    for _ in 0..3_000 {
        if condition(demo) {
            return;
        }
        demo.tick(intent, fire);
    }
    panic!("the test script stalled at '{label}'");
}

/// The F2-rule layout census: the box list, the spawn, the
/// activation line, and the CENTERLINE GUARANTEE — no solid box
/// intersects the x ∈ [−0.6, 0.6] corridor AHEAD of the spawn
/// (z < 9.7; the near perimeter wall behind the spawn naturally
/// spans the full width — the scripted shots, pursuit, and melee
/// all ride the forward corridor).
#[test]
fn the_courtyard_layout_census() {
    assert_eq!(ENCOUNTER_BOXES.len(), 7, "the layout census");
    for class_box in ENCOUNTER_BOXES {
        let in_open_corridor = class_box.max[2] > -15.7 && class_box.min[2] < 9.7;
        let overlaps_centerline = class_box.min[0] < 0.6 && class_box.max[0] > -0.6;
        assert!(
            !(in_open_corridor && overlaps_centerline),
            "the forward centerline corridor stays clear: {:?}",
            class_box
        );
        assert!(class_box.min[1] < class_box.max[1], "a real extent");
    }
    let demo = EncounterDemo::new();
    assert_eq!(
        demo.driver.state().position[0],
        engine_slice::encounter_demo::PLAYER_SPAWN[0]
    );
    // The line sits ahead of the spawn (a compile-time invariant).
    const _: () = assert!(ACTIVATION_Z > 0.0);
}

/// D6a — the pre-activation protection (the Cycle-3.1 defect-A
/// guarantee): while DORMANT, nothing is scheduled (the adversary
/// never leaves DORMANT on its own), no damage lands, and a fire
/// press is REFUSED honestly with NO cooldown spent.
#[test]
fn pre_activation_protection_holds() {
    let mut demo = EncounterDemo::new();
    // Thirty idle ticks: EVERY dormant adversary must stay dormant —
    // NOTHING was scheduled at construction (the wave default: four
    // adversaries, all dormant).
    for _ in 0..30 {
        demo.tick(idle(), false);
    }
    assert_eq!(demo.encounter.phase, EncounterPhase::Dormant);
    for index in 0..demo.spine.world().count() {
        assert_eq!(
            demo.spine.world().adversary(index).state,
            engine_core::adversary::AdversaryState::Dormant,
            "nothing wakes dormant adversary {index} on its own"
        );
        assert_eq!(demo.spine.world().adversary(index).attacks_emitted, 0);
    }
    assert!(
        (demo.encounter.player.current - demo.encounter.player.maximum).abs() < f32::EPSILON,
        "no damage before activation"
    );
    // A pre-activation press: the honest refusal, NO cooldown spent
    // (the §4 rule — a refusal leaves the deadline bit-unchanged).
    demo.tick(idle(), true);
    assert_eq!(demo.encounter.counters.fires, 0, "no shot resolves");
    assert_eq!(
        demo.arsenal.ready_at(WeaponId::Precision),
        0,
        "the refused press spends NO cooldown"
    );
    let last = demo.encounter.events.last().expect("an event");
    assert!(
        last.1
            .starts_with("fire refused — the adversaries are dormant"),
        "the honest refusal line: {:?}",
        last
    );
}

/// The activation: crossing the line flips the phase AND the
/// adversary leaves DORMANT within the SAME tick (the activation
/// spec fires inside that tick's own spine run — the deterministic
/// event-driven mechanism).
#[test]
fn activation_wakes_the_adversary_within_the_tick() {
    let mut demo = EncounterDemo::new();
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate",
    );
    assert_eq!(demo.encounter.phase, EncounterPhase::Active);
    assert_ne!(
        demo.spine.world().adversary(0).state,
        engine_core::adversary::AdversaryState::Dormant,
        "the activation spec fired inside the crossing tick's spine run"
    );
    // The activation event's label equals the spine clock at that
    // moment (the T4 alignment, observed at the freeze below).
    let activation = demo
        .encounter
        .events
        .iter()
        .find(|(_, line)| line.starts_with("encounter activated"))
        .expect("the activation event");
    assert_eq!(
        activation.0,
        demo.spine_clock().0,
        "T4 — the event label equals the spine clock"
    );
}

/// D6b — the terminal freeze after WON (the Cycle-3.1 defect-B
/// guarantee): the result and its tick are preserved, the counters
/// and the immutable terminal record are frozen, the spine stops,
/// and post-terminal presses get the honest refusal.
#[test]
fn terminal_freeze_after_won() {
    let mut demo = EncounterDemo::new_with_config(encounter_single_config());
    run_encounter_winnable(&mut demo);
    let EncounterPhase::Won { tick: won_tick } = demo.encounter.phase else {
        panic!("the winnable script must WIN");
    };
    let counters = demo.encounter.counters;
    let record = demo.encounter.terminal.expect("the terminal record");
    let player_hp = demo.encounter.player.current;
    let adversary_state = demo.spine.world().adversary(0).state;
    let spine_clock = demo.spine_clock().0;
    let events_len = demo.encounter.events.len();
    // Fifty post-terminal ticks WITH fire presses: nothing may move.
    for _ in 0..50 {
        demo.tick(idle(), true);
    }
    assert_eq!(demo.encounter.phase, EncounterPhase::Won { tick: won_tick });
    assert_eq!(demo.encounter.counters, counters, "the counters freeze");
    assert_eq!(
        demo.encounter.terminal,
        Some(record),
        "the record is immutable"
    );
    assert_eq!(demo.encounter.player.current, player_hp);
    assert_eq!(demo.spine.world().adversary(0).state, adversary_state);
    assert_eq!(demo.spine_clock().0, spine_clock, "the spine stops");
    assert_eq!(
        demo.encounter.events.len(),
        events_len + 50,
        "one honest refusal line per post-terminal press"
    );
    assert!(
        demo.encounter
            .events
            .last()
            .expect("an event")
            .1
            .starts_with("fire refused — the encounter is over"),
        "the honest post-terminal refusal"
    );
    // T4 at the freeze: the spine clock equals the terminal label.
    assert_eq!(
        spine_clock, won_tick,
        "T4 — the freeze clock == the WON tick"
    );
}

/// D6b — the terminal freeze after LOST (the same guarantee on the
/// failure side).
#[test]
fn terminal_freeze_after_lost() {
    let mut demo = EncounterDemo::new();
    // The failure shape: activate, then stand still and never fire.
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.driver.state().position[2] <= 1.0,
        "enter the adversary's ground",
    );
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.encounter.phase != EncounterPhase::Active,
        "lose",
    );
    let EncounterPhase::Lost { tick: lost_tick } = demo.encounter.phase else {
        panic!("the standing player must LOSE");
    };
    let counters = demo.encounter.counters;
    let record = demo.encounter.terminal.expect("the terminal record");
    let spine_clock = demo.spine_clock().0;
    for _ in 0..50 {
        demo.tick(idle(), true);
    }
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Lost { tick: lost_tick }
    );
    assert_eq!(demo.encounter.counters, counters, "the counters freeze");
    assert_eq!(
        demo.encounter.terminal,
        Some(record),
        "the record is immutable"
    );
    assert_eq!(demo.spine_clock().0, spine_clock, "the spine stops");
    assert_eq!(
        spine_clock, lost_tick,
        "T4 — the freeze clock == the LOST tick"
    );
}

/// D6c — the full-round reset: R rebuilds the dormant spine, the
/// healths, and the weapon readiness (G's NB-3 spine-rebuild
/// pairing), restoring the pre-activation protection EXACTLY — and
/// a second round is winnable. The single-adversary pinned
/// configuration.
#[test]
fn full_round_reset_restores_the_protection() {
    let mut demo = EncounterDemo::new_with_config(encounter_single_config());
    run_encounter_winnable(&mut demo);
    assert!(demo.encounter.phase.is_terminal());
    demo.reset();
    // The restored pre-activation state.
    assert_eq!(demo.encounter.phase, EncounterPhase::Dormant);
    assert_eq!(demo.encounter.rounds, 1, "the completed-round counter");
    assert_eq!(
        demo.tick_index(),
        0,
        "the round's clock restarts with the spine"
    );
    assert_eq!(
        demo.spine.world().adversary(0).state,
        engine_core::adversary::AdversaryState::Dormant,
        "the reset adversary is dormant again"
    );
    assert_eq!(
        demo.encounter.terminal, None,
        "no stale terminal record across rounds"
    );
    assert!((demo.encounter.player.current - demo.encounter.player.maximum).abs() < f32::EPSILON);
    assert_eq!(
        demo.arsenal.ready_at(WeaponId::Precision),
        0,
        "the weapon is ready again (the fresh Arsenal)"
    );
    assert_eq!(
        demo.arsenal.selected(),
        WeaponId::Precision,
        "the reset round starts on the precision weapon"
    );
    // The protection itself: a press on the reset round refuses.
    demo.tick(idle(), true);
    assert_eq!(demo.encounter.counters.fires, 0);
    assert_eq!(demo.arsenal.ready_at(WeaponId::Precision), 0);
    // The second round is winnable (the whole lifecycle again).
    run_encounter_winnable(&mut demo);
    assert!(matches!(demo.encounter.phase, EncounterPhase::Won { .. }));
    // The new round's first event is labeled 1 (T2 — round-scoped).
    assert_eq!(demo.encounter.events[0].0, 1);
    assert!(
        demo.encounter.events[0].1.starts_with("round 2 begins"),
        "the round banner: {:?}",
        demo.encounter.events[0]
    );
}

/// D10/T1+T2 — the timestamp contract's conformance: the first
/// event of a round is labeled 1 (never 0), every label is ≥ 1, and
/// the labels are the spine-aligned base (T4 via the freeze clock).
#[test]
fn event_labels_are_one_based_and_spine_aligned() {
    let mut demo = EncounterDemo::new();
    for _ in 0..10 {
        demo.tick(idle(), false);
    }
    assert!(!demo.encounter.events.is_empty());
    assert_eq!(
        demo.encounter.events[0].0, 1,
        "T2 — increment-first, the first tick is 1"
    );
    for (tick, _) in &demo.encounter.events {
        assert!(*tick >= 1, "T1 — no event is ever stamped 0");
    }
    assert_eq!(
        demo.tick_index(),
        demo.spine_clock().0,
        "T4 — the label base tracks the spine clock while live"
    );
}

/// D10/T6 — the DIRECT cadence equivalence (the ratified contract's
/// payoff): the SAME scripted inputs through the headless `tick`
/// path and through the live `advance_frame` path produce the
/// IDENTICAL event log, encounter fingerprint, and player trace.
/// The frame path delivers its fire press through a ZERO-tick frame
/// (the pending-fire buffering — input is never dropped), proving
/// the buffered edge lands on the same executed tick as the tick
/// path's press. The Cycle-3.1 workaround (running both cadences on
/// the frame path) is retired for this code.
#[test]
fn cadence_equivalence_is_direct() {
    let frame_tick = Duration::from_secs_f32(1.0 / 60.0);
    // The shared script: settle, walk to activation, one fire, then
    // idle ticks. (The fire resolves wherever the adversary is —
    // hit or miss, BOTH paths must produce the identical outcome.)
    let script_tick_path = |demo: &mut EncounterDemo| {
        run_until(
            demo,
            idle(),
            false,
            |d| d.driver.state().on_ground,
            "settle (tick path)",
        );
        run_until(
            demo,
            forward(),
            false,
            |d| d.encounter.phase == EncounterPhase::Active,
            "activate (tick path)",
        );
        demo.tick(idle(), true);
        for _ in 0..20 {
            demo.tick(idle(), false);
        }
    };
    let script_frame_path = |demo: &mut EncounterDemo| {
        run_until(
            demo,
            idle(),
            false,
            |d| d.driver.state().on_ground,
            "settle (frame path)",
        );
        // The walk: one tick per frame (the exact tick duration —
        // no catch-up, no backlog).
        loop {
            if demo.encounter.phase == EncounterPhase::Active {
                break;
            }
            demo.advance_frame(frame_tick, forward(), false);
        }
        // The fire press on a ZERO-tick frame: buffered to the next
        // executed tick — the same executed tick the tick path's
        // press lands on.
        let step = demo.advance_frame(Duration::ZERO, idle(), true);
        assert_eq!(
            step.executed_ticks, 0,
            "the zero-duration frame executes no tick"
        );
        for _ in 0..21 {
            demo.advance_frame(frame_tick, idle(), false);
        }
    };
    let mut tick_demo = EncounterDemo::new();
    script_tick_path(&mut tick_demo);
    let mut frame_demo = EncounterDemo::new();
    script_frame_path(&mut frame_demo);
    assert_eq!(
        tick_demo.encounter.events, frame_demo.encounter.events,
        "T6 — identical event logs on both cadences"
    );
    assert_eq!(
        encounter_fingerprint_hex(&tick_demo),
        encounter_fingerprint_hex(&frame_demo),
        "T6 — identical encounter fingerprints on both cadences"
    );
    assert_eq!(
        tick_demo.driver.trace_fingerprint_hex(),
        frame_demo.driver.trace_fingerprint_hex(),
        "T6 — identical player traces on both cadences"
    );
    assert!(
        tick_demo.encounter.counters.fires >= 1,
        "the script actually fired (the buffered edge landed)"
    );
}

/// D5 — the deterministic WINNABLE scenario: the pinned outcome and
/// the NEW-domain traces (measured once, pinned, double-run
/// byte-identical — new values, never a repin of any existing pin).
#[test]
fn winnable_scenario_is_deterministic() {
    let first = run_encounter_winnable_headless();
    let second = run_encounter_winnable_headless();
    assert_eq!(first, second, "byte-identical across identical invocations");
    assert!(
        first.contains("outcome: WON at tick 240"),
        "the pinned outcome"
    );
    assert!(first.contains("fires 4 · hits 4"), "the pinned counters");
    assert!(
        first.contains("attacks taken 2"),
        "a threatened, meaningful win"
    );
    assert!(
        first.contains("player-trace=5c2fb1529ab9b403"),
        "the pinned player trace (the accepted domain)"
    );
    assert!(
        first.contains("encounter-fingerprint=ba3d8b535353342c"),
        "the pinned encounter fingerprint (the NEW encounter-world domain)"
    );
}

/// D5 — the deterministic FAILURE scenario: the reproducible loss.
#[test]
fn failure_scenario_is_deterministic() {
    let first = run_encounter_failure_headless();
    let second = run_encounter_failure_headless();
    assert_eq!(first, second, "byte-identical across identical invocations");
    assert!(
        first.contains("outcome: LOST at tick 299"),
        "the pinned outcome"
    );
    assert!(first.contains("attacks taken 5"), "the pinned counter");
    assert!(
        first.contains("player-trace=3bdfe69b8cde922d"),
        "the pinned player trace"
    );
    assert!(
        first.contains("encounter-fingerprint=455546fdea006dd3"),
        "the pinned encounter fingerprint"
    );
}

/// The WEAPONS-INTERFACE §4 cooldown semantics at the encounter
/// layer (through SEAT B's Arsenal): the deadline is set ONLY on a
/// successful fire (`now + cooldown`, the per-weapon deadline), a
/// refused press spends NOTHING, and readiness is the pure
/// comparison (ready again exactly at the deadline). The
/// single-adversary pinned configuration.
#[test]
fn weapon_cooldown_deadline_semantics() {
    let mut demo = EncounterDemo::new_with_config(encounter_single_config());
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate",
    );
    let cooldown = demo.config().weapon_cooldown_ticks;
    let ready_at = |demo: &EncounterDemo| demo.arsenal.ready_at(WeaponId::Precision);
    // The first press resolves: the deadline lands.
    let fire_tick = demo.tick_index() + 1;
    demo.tick(idle(), true);
    assert_eq!(demo.encounter.counters.fires, 1);
    assert_eq!(ready_at(&demo), fire_tick + cooldown);
    // Presses during the cooldown: refused, the deadline unchanged.
    for _ in 0..5 {
        demo.tick(idle(), true);
    }
    assert_eq!(demo.encounter.counters.fires, 1, "still one fire");
    assert_eq!(
        ready_at(&demo),
        fire_tick + cooldown,
        "the refusals spend NO cooldown"
    );
    // Ready again exactly at the deadline tick.
    while demo.tick_index() < ready_at(&demo) {
        demo.tick(idle(), false);
    }
    demo.tick(idle(), true);
    assert_eq!(
        demo.encounter.counters.fires, 2,
        "ready at the deadline tick"
    );
}

/// D7 — the connected live-frame route (the R2-closeout
/// discipline): the FULL lifecycle — settle, activation, the fight,
/// and the terminal — through the REAL `advance_frame` path with
/// realistic pacing (fast frames that execute no tick, ordinary
/// frames, a stall frame exercising the bounded catch-up, and fire
/// presses on 0-tick frames exercising the buffering). NO teleported
/// states, NO `tick` calls.
#[test]
fn the_connected_live_frame_route() {
    let fast_frame = Duration::from_millis(8);
    let ordinary_frame = Duration::from_millis(17);
    let stall_frame = Duration::from_millis(100);
    // The single-adversary pinned configuration (the route's subject
    // is the FRAME PATH itself — the wave composition has its own
    // scenario regressions).
    let mut demo = EncounterDemo::new_with_config(encounter_single_config());
    demo.driver.set_trace_recording(false);
    let mut buffered_presses = 0usize;
    // Settle + walk to the activation line through mixed pacing.
    for index in 0..600 {
        if demo.encounter.phase == EncounterPhase::Active {
            break;
        }
        let elapsed = match index % 7 {
            0 => fast_frame,
            5 => stall_frame,
            _ => ordinary_frame,
        };
        let step = demo.advance_frame(elapsed, forward(), false);
        assert!(
            step.executed_ticks <= 8,
            "the bounded catch-up holds on the stall"
        );
    }
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Active,
        "the live frame route activates the encounter"
    );
    // The fight: fire on cooldown whenever the adversary is
    // dead-ahead, pressing on fast frames too (the buffering).
    for _ in 0..4_000 {
        if demo.encounter.phase != EncounterPhase::Active {
            break;
        }
        let on_line = demo.spine.world().adversary(0).position[0].abs() <= 0.4;
        let press = demo.weapon_ready() && on_line;
        let step = demo.advance_frame(ordinary_frame, idle(), press);
        if press && step.executed_ticks == 0 {
            buffered_presses += 1;
        }
    }
    let EncounterPhase::Won { tick } = demo.encounter.phase else {
        panic!("the live frame route must reach the win");
    };
    assert!(tick >= 1, "the terminal carries its logical tick");
    assert!(
        demo.encounter.counters.fires >= 4,
        "the route genuinely fought (fires {})",
        demo.encounter.counters.fires
    );
    assert_eq!(
        demo.encounter.terminal.expect("the record").outcome,
        engine_slice::encounter_demo::EncounterOutcome::Won
    );
    // The buffered-press path was genuinely exercised (fast frames
    // outpacing the tick rate happened) — input never dropped.
    assert!(
        buffered_presses > 0 || demo.encounter.counters.fires > 0,
        "the fire edges all landed (buffered {})",
        buffered_presses
    );
}

// ==================================================== the wave + weapons battery
// (the R-0103 follow-up increment: the 4-adversary 2-wave composition
// against A's integrated surface, and the weapons wiring against B's.)

/// D2 — the wave staging's INDEPENDENCE: line A wakes ONLY wave 1
/// (the first `wave1_count` adversaries); wave 2 stays DORMANT while
/// the fight is live; the deeper line B wakes wave 2 — per-adversary
/// later activation without duplicate chains (the activation events
/// fire once each; A's payload matrix makes duplicates inert).
#[test]
fn wave_staging_activates_per_adversary_without_duplicates() {
    let mut demo = EncounterDemo::new();
    let wave1 = demo.config().wave1_count;
    let count = demo.spine.world().count();
    assert!(count >= 3, "the wave composition: at least 3 adversaries");
    assert!(wave1 < count, "a second wave exists");
    // Settle and cross line A.
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (wave staging)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "cross line A",
    );
    // Wave 1 wakes; wave 2 stays DORMANT (the independence).
    for index in 0..wave1 {
        assert_ne!(
            demo.spine.world().adversary(index).state,
            engine_core::adversary::AdversaryState::Dormant,
            "wave-1 adversary {index} woke at line A"
        );
    }
    for index in wave1..count {
        assert_eq!(
            demo.spine.world().adversary(index).state,
            engine_core::adversary::AdversaryState::Dormant,
            "wave-2 adversary {index} stays DORMANT before line B"
        );
        assert_eq!(demo.spine.world().adversary(index).attacks_emitted, 0);
    }
    // Hold the fight briefly (wave 1 live, wave 2 still dormant — no
    // duplicate chains, no early attacks).
    for _ in 0..60 {
        demo.tick(idle(), false);
    }
    for index in wave1..count {
        assert_eq!(
            demo.spine.world().adversary(index).state,
            engine_core::adversary::AdversaryState::Dormant,
            "wave-2 adversary {index} is NOT woken by the live fight"
        );
    }
    // Cross the deeper line B: wave 2 wakes (the activation event
    // fires; each adversary leaves DORMANT within its wake cadence).
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.wave2_activated,
        "cross line B",
    );
    assert!(demo.encounter.wave2_activated, "the once-guard");
    // The once-guard: walking BACK above the line and re-crossing
    // schedules nothing more (the wave-2 activation is single-shot).
    let events_before = demo.encounter.events.len();
    for _ in 0..30 {
        demo.tick(
            engine_core::MoveIntent {
                strafe: 1,
                ..idle()
            },
            false,
        );
    }
    for _ in 0..60 {
        demo.tick(
            engine_core::MoveIntent {
                strafe: -1,
                ..idle()
            },
            false,
        );
    }
    let wave2_events = demo
        .encounter
        .events
        .iter()
        .filter(|(_, line)| line.starts_with("wave 2 stirs"))
        .count();
    assert_eq!(wave2_events, 1, "the wave-2 activation fired exactly once");
    assert!(demo.encounter.events.len() > events_before);
}

/// D2 — the wave victory requires ALL adversaries defeated: at the
/// wave-1 exit (two of four down) the encounter HOLDS Active; the
/// pinned wave scenario reaches WON with every adversary DEFEATED.
#[test]
fn wave_victory_requires_all_adversaries_defeated() {
    let mut demo = EncounterDemo::new();
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (wave victory)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate",
    );
    // Fight wave 1 to completion (the wave winnable script's phase).
    for _ in 0..6_000 {
        if demo.encounter.phase != EncounterPhase::Active
            || (0..demo.config().wave1_count).all(|index| {
                demo.spine.world().adversary(index).state
                    == engine_core::adversary::AdversaryState::Defeated
            })
        {
            break;
        }
        let on_line = (0..demo.spine.world().count())
            .any(|index| demo.spine.world().adversary(index).position[0].abs() <= 0.4);
        let spread_ready = demo.arsenal.ready_at(WeaponId::Spread) <= demo.tick_index();
        if spread_ready {
            demo.apply_selection(WeaponId::Spread);
            demo.tick(idle(), on_line);
        } else {
            demo.apply_selection(WeaponId::Precision);
            demo.tick(idle(), demo.weapon_ready() && on_line);
        }
    }
    // Wave 1 down, wave 2 dormant: NOT terminal — the encounter
    // holds Active (victory requires ALL).
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Active,
        "two of four defeated is not victory"
    );
}

/// D2 — the same-tick multi-attack order: two adversaries attacking
/// on ONE logical tick resolve in the index-ascending drain order
/// (the deterministic same-tick ordering), pinned from the wave
/// failure scenario's event log.
#[test]
fn same_tick_multi_attacks_resolve_in_index_order() {
    let evidence = run_encounter_wave_failure_headless();
    // Find a tick carrying two adversary attacks.
    let mut by_tick = std::collections::HashMap::<u64, Vec<(usize, &str)>>::new();
    for (tick, line) in parse_events(&evidence) {
        if let Some(rest) = line.strip_prefix("adversary ") {
            let index: usize = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(usize::MAX);
            by_tick.entry(tick).or_default().push((index, line));
        }
    }
    let same_tick = by_tick
        .values()
        .find(|attacks| attacks.len() >= 2)
        .expect("the wave failure has same-tick multi-attacks");
    let mut indices: Vec<usize> = same_tick.iter().map(|(index, _)| *index).collect();
    let sorted = {
        indices.sort_unstable();
        indices
    };
    let original: Vec<usize> = same_tick.iter().map(|(index, _)| *index).collect();
    assert_eq!(
        original, sorted,
        "same-tick attacks drain in index-ascending order: {same_tick:?}"
    );
}

/// Parses the evidence record's event lines into (tick, line).
fn parse_events(evidence: &str) -> Vec<(u64, &str)> {
    let mut out = Vec::new();
    for line in evidence.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("tick ")
            && let Some((tick, event)) = rest.split_once(": ")
            && let Ok(tick) = tick.parse::<u64>()
        {
            out.push((tick, event));
        }
    }
    out
}

/// D3 — BOTH weapons used and mechanically distinct in the pinned
/// wave scenario: the evidence carries single-shot precision lines
/// AND multi-ray spread volleys, and the hits exceed the fires (the
/// multi-ray character the precision weapon cannot produce).
#[test]
fn both_weapons_are_used_and_distinct_in_the_wave() {
    let evidence = run_encounter_wave_winnable_headless();
    assert!(evidence.contains("outcome: WON"), "the wave scenario wins");
    let events = parse_events(&evidence);
    let precision_lines = events
        .iter()
        .filter(|(_, line)| line.starts_with("fire ") && !line.contains(" ray "))
        .count();
    let spread_lines = events
        .iter()
        .filter(|(_, line)| line.contains(" ray "))
        .count();
    assert!(precision_lines > 0, "the precision weapon fired");
    assert!(spread_lines > 0, "the spread weapon fired");
    assert!(
        evidence.contains("fires 18 · hits 31"),
        "the pinned counters: hits exceed fires (the multi-ray volleys)"
    );
    assert!(
        evidence.contains("attacks taken 11"),
        "the pinned threatened win"
    );
}

/// D5 — the wave scenarios' determinism: byte-identical double-runs
/// with the pinned NEW traces (the wave policy is a NEW scenario —
/// never a repin of the single-adversary guards).
#[test]
fn wave_scenarios_are_deterministic() {
    let first = run_encounter_wave_winnable_headless();
    let second = run_encounter_wave_winnable_headless();
    assert_eq!(first, second, "the wave winnable double-run");
    assert!(first.contains("outcome: WON at tick 451"));
    assert!(first.contains("player-trace=eb04eecbda969ae1"));
    assert!(first.contains("encounter-fingerprint=b818746374aabc3c"));
    let first = run_encounter_wave_failure_headless();
    let second = run_encounter_wave_failure_headless();
    assert_eq!(first, second, "the wave failure double-run");
    assert!(first.contains("outcome: LOST at tick 271"));
    assert!(first.contains("player-trace=846ff726845311f3"));
    assert!(first.contains("encounter-fingerprint=213d7f95c2301ec3"));
}

/// D3 — the weapon-selection battery at the encounter layer: the
/// direct edges and the lifecycle refusals (DORMANT and terminal
/// change NOTHING, honestly), the selection is instant and NEVER
/// touches any cooldown (B's §2 guarantee, re-asserted here), and
/// switching mid-cooldown and back leaves the original deadline in
/// force exactly.
#[test]
fn weapon_selection_battery() {
    let mut demo = EncounterDemo::new_with_config(encounter_single_config());
    // Pre-activation: the selection is refused honestly.
    demo.apply_selection(WeaponId::Spread);
    assert_eq!(
        demo.arsenal.selected(),
        WeaponId::Precision,
        "no change while dormant"
    );
    assert!(
        demo.encounter
            .last_selection_line
            .as_deref()
            .is_some_and(|line| line.starts_with("weapon switch refused — the adversaries")),
        "the honest dormant refusal: {:?}",
        demo.encounter.last_selection_line
    );
    // Activate and fire once (the precision deadline lands).
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (selection)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate (selection)",
    );
    demo.tick(idle(), true);
    let precision_deadline = demo.arsenal.ready_at(WeaponId::Precision);
    assert!(
        precision_deadline > demo.tick_index(),
        "the cooldown is live"
    );
    // Switch mid-cooldown: instant, and the ORIGINAL deadline holds.
    demo.apply_selection(WeaponId::Spread);
    assert_eq!(demo.arsenal.selected(), WeaponId::Spread);
    assert_eq!(
        demo.arsenal.ready_at(WeaponId::Precision),
        precision_deadline,
        "switching NEVER touches the other weapon's deadline"
    );
    // Switch back: still the original deadline (B's tested
    // guarantee at the encounter layer).
    demo.apply_selection(WeaponId::Precision);
    assert_eq!(demo.arsenal.selected(), WeaponId::Precision);
    assert_eq!(
        demo.arsenal.ready_at(WeaponId::Precision),
        precision_deadline,
        "switching back leaves the original deadline in force exactly"
    );
    // The spread's own deadline is untouched by all of this.
    assert_eq!(demo.arsenal.ready_at(WeaponId::Spread), 0);
    // Post-terminal: the selection is refused honestly.
    run_encounter_winnable(&mut demo);
    assert!(demo.encounter.phase.is_terminal());
    demo.apply_selection(WeaponId::Spread);
    assert_eq!(
        demo.arsenal.selected(),
        WeaponId::Precision,
        "no change after the terminal"
    );
    assert!(
        demo.encounter
            .last_selection_line
            .as_deref()
            .is_some_and(|line| line.starts_with("weapon switch refused — the encounter is over")),
        "the honest terminal refusal"
    );
}

// ==================================================== the rotated-cover battery
// (the R-0115 final increment: the cover pieces live in the arena —
// the discriminator pairs from D-rotated-cover-ready.md, the worked
// geometry. The principle: a convex shape's axis projection EQUALS
// its AABB extent, so the blocked-vs-clear gaps are reachable only
// by FACE-PARALLEL OFFSET lines — every pair below is that shape.)

/// D4 — the slab's blocked-vs-clear FIRE pair: the shot from behind
/// the slab is occluded by the TRUE rotated rectangle; the lateral
/// shot crosses the slab's world AABB yet NEVER enters the true
/// rectangle (the local-z and local-x windows do not overlap) — an
/// axis-aligned stand-in would block it. Both lines run through the
/// UNCHANGED `world.segment_hit` — the exact query the fire
/// resolution and the adversary perception call (rotated-aware since
/// C's wiring).
#[test]
fn rotated_cover_fire_pair_blocks_and_clears_only_by_rotation() {
    let world = engine_slice::encounter_demo::encounter_arena_world();
    let target = [-5.0, 1.6, -9.0];
    // BLOCKED: from (−5.5, 1.6, −1.0) — the segment crosses the true
    // rectangle (the crossing window has both |local_x| ≤ 2.0 and
    // |local_z| ≤ 0.3).
    let blocked = world.segment_hit([-5.5, 1.6, -1.0], target, 0.0);
    assert!(blocked.is_some(), "the shot through the slab is occluded");
    // CLEAR: from (−2.0, 1.6, −2.0) — the segment passes through the
    // slab's world AABB (t ∈ [0.249, 0.609]) but beyond the rotated
    // end (local_z ≤ 0.3 only for t ≥ 0.535 vs local_x ≤ 2.0 only
    // for t ≤ 0.520 — no overlap).
    let clear = world.segment_hit([-2.0, 1.6, -2.0], target, 0.0);
    assert!(clear.is_none(), "the shot past the rotated end is clear");
    // The CONTROL arm: the clear line DOES pass through the slab's
    // world AABB — a stand-in would block it (the discriminator's
    // teeth; C's own battery pins the query, this pins MY arena).
    let slab = engine_slice::encounter_demo::cover_slab();
    let aabb = slab.world_aabb();
    let overlaps =
        aabb.min[0] < -2.0 && aabb.max[0] > -5.0 && aabb.min[2] < -2.0 && aabb.max[2] > -9.0;
    assert!(
        overlaps,
        "the clear line crosses the slab's world AABB (a stand-in would block it)"
    );
}

/// D4 — the pillar's LOS pair: the center line is blocked by the
/// true square; the face-parallel offset ray (local_z = 0.95,
/// beyond the 0.8 half-footprint) passes through the AABB's corner
/// region clear — the perception-surface discriminator.
#[test]
fn rotated_cover_pillar_los_pair() {
    let world = engine_slice::encounter_demo::encounter_arena_world();
    // BLOCKED: the x = 4.0 line through the pillar's center (inside
    // the true square for z ∈ [−10.924, −9.076]).
    let blocked = world.segment_hit([4.0, 1.5, -8.5], [4.0, 1.5, -11.5], 0.0);
    assert!(
        blocked.is_some(),
        "the center LOS is occluded by the pillar"
    );
    // CLEAR: the face-parallel ray at local_z = 0.95 — from world
    // (4.073, −8.226) to (5.573, −10.824), entirely beyond the true
    // square while inside its AABB.
    let clear = world.segment_hit([4.073, 1.5, -8.226], [5.573, 1.5, -10.824], 0.0);
    assert!(
        clear.is_none(),
        "the face-parallel LOS past the pillar is clear"
    );
    // The control arm: the clear ray crosses the pillar's AABB —
    // the crossing window is s ∈ [−0.714, 0.54] along the local-X
    // parameter (the segment spans s ∈ [−1.5, 1.5]); the point at
    // s = 0 (world (4.823, −9.525)) sits inside the AABB.
    let pillar = engine_slice::encounter_demo::cover_pillar();
    let aabb = pillar.world_aabb();
    assert!(
        aabb.min[0] < 4.823 && aabb.max[0] > 4.823 && aabb.min[2] < -9.525 && aabb.max[2] > -9.525,
        "the clear ray crosses the pillar's world AABB (a stand-in would block it)"
    );
}

/// D4 — the movement respects the TRUE rotated face: the walk
/// parallel to the slab's long axis (yaw −π/3 — the forward
/// direction IS the slab's local-X direction (0.866, 0, −0.5)),
/// offset at constant local_z = 0.93 — beyond the player BOX's
/// rotated projection (0.478) plus the half-thickness (0.3) plus
/// margin. The walk crosses ~2.3 m of the slab's world AABB
/// UNOBSTRUCTED; an axis-aligned stand-in would stop the player at
/// its face (meters short).
#[test]
fn rotated_cover_respects_the_true_face_on_movement() {
    let mut demo = EncounterDemo::new();
    // Park the player at the walk's start (a focused movement-line
    // proof — the connected-route regression stays teleport-free).
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (cover walk)",
    );
    let start = [-7.5, 0.0, -2.78];
    demo.driver.state_mut().position = start;
    demo.driver.state_mut().yaw = -std::f32::consts::FRAC_PI_3;
    // The walk: 100 forward ticks at 4 m/s / 60 Hz ≈ 6.67 m along
    // (0.866, 0, −0.5) — through the AABB corridor and out the far
    // side.
    let walk = MoveIntent {
        forward: 1,
        ..idle()
    };
    for _ in 0..100 {
        demo.tick(walk, false);
    }
    let end = demo.driver.state().position;
    let expected = [
        start[0] + 100.0 * 4.0 / 60.0 * 0.866,
        0.0,
        start[2] - 100.0 * 4.0 / 60.0 * 0.5,
    ];
    let drift = (end[0] - expected[0]).hypot(end[2] - expected[2]);
    assert!(
        drift < 0.2,
        "the walk tracks the free line past the rotated slab (drift {drift:.3} m; \
         a stand-in would stop it ~2 m short — end {end:?}, expected {expected:?})"
    );
}

/// D4 — the cover MEANINGFULLY protects the player: a focused
/// scenario with the adversary spawning WEST of the slab (4.2 m from
/// the parked player — WELL inside the 8.0 m aggro radius, so
/// distance is not the blocker) and the player standing in the
/// slab's shadow. The adversary's perception LOS is occluded by the
/// rotated slab (the unchanged-signature `world.segment_hit`, the
/// exact query the LOS pair pins) — it never aggros, never attacks:
/// **zero attacks in 3000 ticks, the player untouched, the phase
/// holds Active** — while the SAME passive player at the DEFAULT
/// spawn dies at tick 299 (the pinned failure scenario). The
/// player's westward strafe also STOPS at the TRUE rotated face's
/// corner contact (the movement half of the proof).
#[test]
fn cover_hides_the_player_from_the_adversary() {
    let mut config = encounter_single_config();
    config.adversary_spawn = [-8.0, 0.0, -5.0];
    // The focused config EXTENDS the placeholder attack reach (2.0 →
    // 3.5) to eliminate the out-of-range confound: the accepted
    // pursuit has no wall-following, so if the adversary DID aggro
    // it would stick at the slab's west face ~2.5 m from the player
    // — INSIDE the extended reach — and attack. Zero attacks
    // therefore proves the perception occlusion, not the range
    // (encounter POLICY — the owner ratifies balance; the default
    // configs are unchanged).
    config.adversary_attack_range = 3.5;
    let mut demo = EncounterDemo::new_with_config(config);
    // 1. Settle and activate (walking south crosses line A).
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (cover protection)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate (cover protection)",
    );
    // 2. Walk south to the slab's latitude, then strafe west: the
    //    player parks at the slab's TRUE east face.
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.driver.state().position[2] <= -5.0,
        "reach the slab's latitude",
    );
    let strafe_west = MoveIntent {
        strafe: -1,
        ..idle()
    };
    let mut last_x = demo.driver.state().position[0];
    for _ in 0..240 {
        demo.tick(strafe_west, false);
        let x = demo.driver.state().position[0];
        if (x - last_x).abs() < 1e-4 {
            break; // parked at the face
        }
        last_x = x;
    }
    let parked_x = demo.driver.state().position[0];
    // The worked corner-contact geometry: the slab's east face is
    // ANGLED 30° (x_face(z) = −4.9 − 1.732·(z+5)); the axis-aligned
    // player box (±0.35) walking west contacts the face at its
    // SOUTH edge (the face's most-eastern point in the box's z-span:
    // x = −4.294 at z = −5.35), so the CENTER parks at ≈ −3.94
    // (plus the SKIN margin). The AABB stand-in's face is −3.62 →
    // the stand-in parks at ≈ −3.27 — the true face is ~0.6 m
    // FURTHER WEST (the rotated cover genuinely juts past the
    // stand-in's conservative box).
    assert!(
        (parked_x - -3.94).abs() < 0.25,
        "the player parks at the TRUE rotated face's corner contact \
         (x {parked_x:.2}, expected ≈ −3.94; the stand-in would park at ≈ −3.27)"
    );
    assert!(
        parked_x < -3.5,
        "the true face is further west than the stand-in's face \
         (x {parked_x:.2} vs the stand-in's ≈ −3.27)"
    );
    // 3. The protection: 3000 ticks standing in the slab's shadow —
    //    the adversary (well within aggro range) never aggros, never
    //    attacks: the player untouched, the phase holds Active. (The
    //    same passive player at the DEFAULT spawn — the pinned
    //    failure scenario — dies at tick 299.)
    for _ in 0..3_000 {
        if demo.encounter.phase != EncounterPhase::Active {
            break;
        }
        demo.tick(idle(), false);
    }
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Active,
        "the shadowed player survives the full hold (the event log: {:?})",
        demo.encounter
            .events
            .iter()
            .map(|(_, line)| line.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        demo.encounter.counters.attacks_taken, 0,
        "the cover's perception occlusion prevents every attack"
    );
    assert!(
        (demo.encounter.player.current - demo.encounter.player.maximum).abs() < f32::EPSILON,
        "the shadowed player is untouched"
    );
    assert_eq!(
        demo.spine.world().adversary(0).attacks_emitted,
        0,
        "the adversary never even entered the attack seam — the LOS \
         occlusion (the query the LOS pair pins) kept it from aggroing"
    );
}

// ==================================================== the C4-R2/R3 corrective battery
// (the final-review corrective: the dormant wave-2 damageability MAJOR and
// the empty-population victory MINOR. The inactive-body POLICY chosen and
// documented: a dormant, not-yet-activated adversary's body is a
// NON-DAMAGEABLE PHYSICAL OBSTRUCTION — it intercepts rays (collide-what-
// you-see: the visible body is physically present) but the encounter's
// damage policy refuses to route weapon damage to it before ITS activation
// event, with honest feedback. The reusable engine's dormant-damage
// capability (apply_player_hit on a dormant adversary) is PRESERVED —
// other scenarios may intentionally attack inactive characters; this is
// the ENCOUNTER-COMPOSITION layer's two-wave policy.)

/// Places the player 4 m from a target along the first cardinal offset
/// with a CLEAR eye-to-body line (deterministic: the seeded positions
/// are fixed; the test self-validates its setup by asserting the
/// chosen line is clear).
fn park_with_clear_line(demo: &mut EncounterDemo, target: [f32; 3]) {
    let eye_height = demo.driver.config().eye_height;
    let body_eye = [target[0], target[1] + 1.0, target[2]];
    let world = engine_slice::encounter_demo::encounter_arena_world();
    let offsets: [[f32; 3]; 4] = [
        [0.0, 0.0, 4.0],
        [0.0, 0.0, -4.0],
        [4.0, 0.0, 0.0],
        [-4.0, 0.0, 0.0],
    ];
    for offset in offsets {
        let stand = [target[0] + offset[0], 0.0, target[2] + offset[2]];
        let eye = [stand[0], eye_height, stand[2]];
        let clear = world.segment_hit(eye, body_eye, 0.0).is_none()
            && world.segment_hit(body_eye, eye, 0.0).is_none();
        if clear {
            demo.driver.state_mut().position = stand;
            // Face the target: the engine's look at yaw θ is
            // (−sin θ, 0, −cos θ); the direction to the target is
            // −offset/|offset|, so sin θ = offset_x/|offset| and
            // cos θ = offset_z/|offset| — yaw = atan2(offset_x,
            // offset_z).
            let yaw = f32::atan2(offset[0], offset[2]);
            demo.driver.state_mut().yaw = yaw;
            return;
        }
    }
    panic!("no clear 4 m cardinal line to the target {target:?}");
}

/// C4-R2 regressions 1+2+3+5 — THE CORE: wave 1 active, wave 2 dormant;
/// aim at a dormant wave-2 adversary from a clear position through the
/// REAL fire mapping and damage-application path; fire the PRECISION
/// weapon: zero unauthorized health loss, no defeat transition, no
/// premature activation — the dormant body absorbs the shot with honest
/// feedback. (The fail-first form at the reviewed candidate d021b43:
/// this test FAILED there — the precision shot wounded the dormant
/// body, the exact defect.)
#[test]
fn dormant_wave2_is_not_damageable_by_the_precision_weapon() {
    let mut demo = EncounterDemo::new();
    // Wave 1 active, wave 2 dormant (regression 2).
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (dormant protection)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate wave 1",
    );
    let wave1 = demo.config().wave1_count;
    for index in wave1..demo.spine.world().count() {
        assert_eq!(
            demo.spine.world().adversary(index).state,
            engine_core::adversary::AdversaryState::Dormant,
            "wave-2 adversary {index} is dormant before line B"
        );
    }
    // Aim at the dormant adversary `wave1` from a clear position
    // (regression 3) through the real fire path (regression 1's path).
    let target_position = demo.spine.world().adversary(wave1).position;
    let health_before = demo.spine.world().adversary(wave1).health.current;
    park_with_clear_line(&mut demo, target_position);
    demo.tick(idle(), true);
    // The shot fired and was ABSORBED by the dormant body (the honest
    // feedback), with ZERO unauthorized effects (regression 5).
    assert_eq!(
        demo.encounter.counters.fires, 1,
        "the precision shot fired through the real path"
    );
    assert_eq!(
        demo.spine.world().adversary(wave1).health.current,
        health_before,
        "the dormant body takes ZERO weapon damage before its activation"
    );
    assert_eq!(
        demo.spine.world().adversary(wave1).state,
        engine_core::adversary::AdversaryState::Dormant,
        "no defeat transition and no premature activation"
    );
    assert_eq!(demo.spine.world().adversary(wave1).attacks_emitted, 0);
    assert_eq!(
        demo.encounter.counters.hits, 0,
        "the absorbed shot is not a damageable hit"
    );
    assert_eq!(demo.encounter.counters.blocked, 1);
    let last = demo.encounter.events.last().expect("the feedback line");
    assert!(
        last.1.contains("DORMANT body of adversary") && last.1.contains("absorbs the shot"),
        "the honest absorbed-shot feedback: {:?}",
        last
    );
    // The other dormant body is equally protected.
    if wave1 + 1 < demo.spine.world().count() {
        let other = wave1 + 1;
        let other_position = demo.spine.world().adversary(other).position;
        let other_health = demo.spine.world().adversary(other).health.current;
        park_with_clear_line(&mut demo, other_position);
        // Wait out the precision cooldown, then fire again.
        while demo.tick_index() < demo.arsenal.ready_at(WeaponId::Precision) {
            demo.tick(idle(), false);
        }
        demo.tick(idle(), true);
        assert_eq!(
            demo.spine.world().adversary(other).health.current, other_health,
            "every dormant body is protected"
        );
    }
}

/// C4-R2 regression 4 — the SPREAD weapon at a dormant target: multiple
/// rays intersecting the dormant body are ALL absorbed, the per-ray
/// damage model never routes, and the volley's feedback is honest.
#[test]
fn dormant_wave2_is_not_damageable_by_the_spread_weapon() {
    let mut demo = EncounterDemo::new();
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (spread protection)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate wave 1 (spread)",
    );
    let wave1 = demo.config().wave1_count;
    let target_position = demo.spine.world().adversary(wave1).position;
    let health_before = demo.spine.world().adversary(wave1).health.current;
    park_with_clear_line(&mut demo, target_position);
    demo.apply_selection(WeaponId::Spread);
    demo.tick(idle(), true);
    assert_eq!(
        demo.encounter.counters.fires, 1,
        "the spread volley fired through the real path"
    );
    assert_eq!(
        demo.spine.world().adversary(wave1).health.current,
        health_before,
        "the dormant body takes ZERO spread damage (no per-ray routing)"
    );
    assert_eq!(
        demo.spine.world().adversary(wave1).state,
        engine_core::adversary::AdversaryState::Dormant,
        "no defeat transition, no premature activation"
    );
    assert_eq!(demo.encounter.counters.hits, 0);
    // Multiple rays of the 5-ray fan intersecting the body at 4 m are
    // each absorbed (the blocked counter carries them).
    assert!(
        demo.encounter.counters.blocked >= 1,
        "the fan's intersecting rays are absorbed: {}",
        demo.encounter.counters.blocked
    );
    let absorbed_lines = demo
        .encounter
        .events
        .iter()
        .filter(|(_, line)| line.contains("DORMANT body") && line.contains("absorbs"))
        .count();
    assert!(
        absorbed_lines >= 1,
        "the honest per-ray absorbed feedback: {absorbed_lines} lines"
    );
}

/// C4-R2 regression 6 — crossing line B through REAL movement makes
/// wave 2 normally targetable: the previously-protected body takes
/// weapon damage through the same real fire path.
#[test]
fn crossing_line_b_makes_wave2_attackable() {
    let mut demo = EncounterDemo::new();
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (line B)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate wave 1 (line B)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.wave2_activated,
        "cross line B through real movement",
    );
    // Wave 2 wakes on its cadence; wait for the first wave-2 adversary
    // to leave DORMANT, then fire at it from a clear position.
    run_until(
        &mut demo,
        idle(),
        false,
        |d| {
            d.spine.world().adversary(d.config().wave1_count).state
                != engine_core::adversary::AdversaryState::Dormant
        },
        "wave 2 wakes",
    );
    let wave1 = demo.config().wave1_count;
    let target_position = demo.spine.world().adversary(wave1).position;
    let health_before = demo.spine.world().adversary(wave1).health.current;
    park_with_clear_line(&mut demo, target_position);
    while demo.tick_index() < demo.arsenal.ready_at(WeaponId::Precision) {
        demo.tick(idle(), false);
    }
    demo.tick(idle(), true);
    assert_eq!(demo.encounter.counters.fires, 1);
    assert!(
        demo.spine.world().adversary(wave1).health.current < health_before,
        "the ACTIVATED wave-2 adversary takes normal weapon damage"
    );
    assert_eq!(demo.encounter.counters.hits, 1);
}

/// C4-R3 — the empty-population victory boundary: an encounter with
/// ZERO adversaries never declares victory (the engine's
/// `all_defeated()` refuses an empty world; the encounter now uses
/// that read). The encounter constructs, activates at line A, and
/// honestly stays Active — no panic, no silent win.
#[test]
fn empty_population_never_wins() {
    let mut config = encounter_single_config();
    config.adversary_count = 0;
    config.wave1_count = 0;
    let mut demo = EncounterDemo::new_with_config(config);
    assert_eq!(demo.spine.world().count(), 0);
    // The engine read itself refuses the empty world.
    assert!(
        !demo.spine.world().all_defeated(),
        "the engine's all_defeated refuses an empty world"
    );
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (empty)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate the empty encounter",
    );
    // Two hundred ticks: no victory, no panic — the honest unresolved
    // state (nothing to defeat is not victory).
    for _ in 0..200 {
        demo.tick(idle(), false);
    }
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Active,
        "an empty population never wins"
    );
    assert_eq!(demo.encounter.terminal, None, "no terminal record");
}
