//! The P2 sustained-load CI regressions (Cycle 1, SEAT B): the
//! deterministic, logical subset of the amended acceptance matrix —
//! no wall-clock thresholds in CI, no crash-test-seams dependency,
//! platform-uniform (Linux-only memory sampling stays report-side).
//! The exact expected values derive from the measured composition
//! table (1k-mixed real-chain base ≈ 84/tick; 1k-full ≈ 154/tick)
//! and from the pure-synthetic instrument's arithmetic
//! (`population: 0` — demand is exactly the overlay).

use engine_core::LodLevel;

use engine_slice::npc::REGION_COUNT;
use engine_slice::sustained_load::{
    CellSpec, LaneVerdict, WorkloadClass, classify, derived_horizon, lod_scenarios,
    measured_drain_tick, run_cell, run_cell_deterministic,
};

/// The runner's stated cadence-budget policy (period 6): near budget
/// 12, far budget 144 — the classify() inputs the slice assertions
/// reuse.
const NEAR_CADENCE_BUDGET: u64 = 12;
const FAR_CADENCE_BUDGET: u64 = 144;

/// §8.2a — the classifier's overload measurement: a synthetic
/// workload with a KNOWN growth rate (30/tick demand against budget
/// 20 → the backlog grows at exactly 10 overdue events per logical
/// tick) must classify Overloaded with that measured rate in
/// per-LOGICAL-TICK units (R2: never a per-window 500). Demand
/// source: the pure-synthetic instrument (population 0 — no chains,
/// demand is exactly the overlay; the spine's own ledger accounting
/// is the oracle).
#[test]
fn classifier_overload_measures_known_growth_rate() {
    let mut spec = CellSpec::base(WorkloadClass::Overload, 0, lod_scenarios()[0]);
    spec.budget = 20;
    spec.reserve = 8;
    spec.overlay_near = 30;
    spec.horizon = 250;
    let report = run_cell(&spec);
    let slope = report.classification.near_overdue_slope;
    assert!(
        (slope - 10.0).abs() < 0.5,
        "the rate must be ≈10 overdue events per LOGICAL TICK (measured {slope})"
    );
    assert_eq!(report.classification.near, LaneVerdict::Overloaded);
    assert_eq!(
        report.executed, 5_000,
        "execution is budget-capped at exactly 20/tick × 250 ticks"
    );
    assert!(
        report.late_executions > 0,
        "the deferred identities run late"
    );
    assert!(report.conservation_holds);
    let last = report.windows.last().expect("windowed");
    assert!(
        last.near_overdue > 0,
        "the growth is demonstrated, not hidden"
    );
}

/// R2 — gradual unsustainability: a genuinely positive but small
/// long-run growth rate (21/tick demand against budget 20 → exactly
/// +1 overdue event per logical tick) must classify Overloaded with
/// that measured rate — never mislabeled Sustainable because a
/// window-unit threshold swallowed the gradual growth.
#[test]
fn classifier_gradual_overload_reports_small_positive_rate() {
    let mut spec = CellSpec::base(WorkloadClass::Overload, 0, lod_scenarios()[0]);
    spec.budget = 20;
    spec.reserve = 8;
    spec.overlay_near = 21;
    spec.horizon = 250;
    let report = run_cell(&spec);
    let slope = report.classification.near_overdue_slope;
    assert!(
        (slope - 1.0).abs() < 0.2,
        "the gradual rate must report ≈1 overdue event per logical tick (measured {slope})"
    );
    assert_eq!(report.classification.near, LaneVerdict::Overloaded);
    let last = report.windows.last().expect("windowed");
    assert!(last.near_overdue > 0, "the backlog grows without bound");
    assert!(report.conservation_holds);
}

/// §8.2b — the classifier's recovery measurement: the overload arc
/// (30/tick for 100 ticks) then demand falls to zero; the backlog
/// (exactly 1,000 events) drains at 20/tick, every scheduled event
/// executes exactly once, conservation holds throughout, and the
/// verdict sequence across the arc is Overloaded → Recovering →
/// Sustainable.
#[test]
fn classifier_recovery_drains_with_conservation() {
    let mut spec = CellSpec::base(WorkloadClass::Recovery, 0, lod_scenarios()[0]);
    spec.budget = 20;
    spec.reserve = 8;
    spec.overlay_near = 30;
    spec.recovery_at = Some(100);
    spec.horizon = 300;
    let report = run_cell(&spec);
    assert_eq!(report.scheduled, 3_000, "30/tick × 100 ticks");
    assert_eq!(
        report.executed, 3_000,
        "the arc drains fully: every scheduled identity executes exactly once"
    );
    let last = report.windows.last().expect("windowed");
    assert_eq!(last.near_overdue, 0, "zero overdue backlog at the end");
    assert_eq!(last.pending_near, 0, "zero pending at the end");
    assert!(report.conservation_holds);
    assert_eq!(
        classify(
            &report.windows[0..2],
            NEAR_CADENCE_BUDGET,
            FAR_CADENCE_BUDGET
        )
        .near,
        LaneVerdict::Overloaded,
        "the arc's overload phase classifies overloaded"
    );
    assert_eq!(
        classify(
            &report.windows[1..3],
            NEAR_CADENCE_BUDGET,
            FAR_CADENCE_BUDGET
        )
        .near,
        LaneVerdict::Recovering,
        "the drain phase classifies recovering"
    );
    assert_eq!(
        classify(
            &report.windows[3..],
            NEAR_CADENCE_BUDGET,
            FAR_CADENCE_BUDGET
        )
        .near,
        LaneVerdict::Sustainable,
        "the post-drain steady state classifies sustainable"
    );
    assert_eq!(
        measured_drain_tick(&report.windows),
        Some(150),
        "backlog 1,000 at tick 100 drains at 20/tick → zero at tick 150"
    );
}

/// §8.2c (the restated guard) — the class-1 cell at 1k-mixed: real
/// chains (measured D_base ≈ 84/tick) plus a 16/tick near overlay ≈
/// the 0.5× budget target. The guard is ZERO OVERDUE BACKLOG (§5 —
/// not raw pending, which legitimately holds the designed pipeline
/// depth), zero budget exhaustion, zero lateness, both lanes
/// Sustainable.
#[test]
fn classifier_low_demand_is_zero_backlog() {
    let mut spec = CellSpec::base(WorkloadClass::LowDemand, 1_000, lod_scenarios()[1]);
    spec.overlay_near = 16;
    spec.horizon = 300;
    let report = run_cell(&spec);
    for (index, window) in report.windows.iter().enumerate() {
        assert_eq!(window.near_overdue, 0, "window {index}: no near overdue");
        assert_eq!(window.far_overdue, 0, "window {index}: no far overdue");
        assert_eq!(
            window.near_backlogged, 0,
            "window {index}: no near backlogged ticks"
        );
        assert_eq!(
            window.far_backlogged, 0,
            "window {index}: no far backlogged ticks"
        );
        assert_eq!(window.budget_exhausted, 0, "window {index}: no exhaustion");
    }
    assert_eq!(report.late_executions, 0, "nothing runs late");
    assert_eq!(report.budget_exhausted_ticks, 0);
    assert_eq!(report.classification.near, LaneVerdict::Sustainable);
    assert_eq!(report.classification.far, LaneVerdict::Sustainable);
    let last = report.windows.last().expect("windowed");
    assert!(
        last.pending_far > 0,
        "far pending is the designed pipeline depth (not-yet-due), never backlog"
    );
    assert!(report.conservation_holds);
}

/// §8.3 — the §5 split end-to-end: the 1k-mixed base with an
/// injected near-lane excess (130/tick overlay → total ≈ 214/tick >
/// the 200 budget) must classify the NEAR lane overloaded (real
/// backlog: overdue grows, near-backlogged ticks accrue) while the
/// FAR lane stays at its designed cadence (zero overdue, zero
/// far-backlogged ticks, its pending is the designed pipeline depth)
/// — reported as separate lane series, never summed.
#[test]
fn mixed_lod_split_far_cadence_vs_near_backlog() {
    let mut spec = CellSpec::base(WorkloadClass::MixedLod, 1_000, lod_scenarios()[1]);
    spec.overlay_near = 130;
    spec.horizon = 300;
    let report = run_cell(&spec);
    assert_eq!(
        report.classification.near,
        LaneVerdict::Overloaded,
        "the injected near excess is real backlog"
    );
    assert_eq!(
        report.classification.far,
        LaneVerdict::Sustainable,
        "the far lanes' low execution rate is BY DESIGN (§5)"
    );
    let last = report.windows.last().expect("windowed");
    assert!(last.near_overdue > 0, "near overdue backlog exists");
    assert!(last.far_overdue == 0, "far overdue backlog is zero");
    assert!(report.near_backlogged_ticks > 0);
    assert_eq!(
        report.far_backlogged_ticks, 0,
        "far never backlogs under the near excess"
    );
    assert!(
        last.pending_far > 0,
        "far pending remains the designed pipeline depth"
    );
    assert!(report.conservation_holds);
}

/// §8.4 — the double-run determinism proof at CI scale: every class,
/// 1k population, two independent constructions of the same
/// specification — byte-identical trace digests and world
/// fingerprints (the same proof the reference matrix runs at every
/// population).
#[test]
fn double_run_determinism_all_classes() {
    let classes = [
        WorkloadClass::LowDemand,
        WorkloadClass::NearSaturation,
        WorkloadClass::Overload,
        WorkloadClass::Recovery,
        WorkloadClass::MixedLod,
        WorkloadClass::ChangingPresence,
    ];
    for class in classes {
        let mut spec = CellSpec::base(class, 1_000, lod_scenarios()[1]);
        spec.horizon = 150;
        match class {
            WorkloadClass::LowDemand => spec.overlay_near = 16,
            WorkloadClass::NearSaturation => spec.overlay_near = 116,
            WorkloadClass::Overload => spec.overlay_near = 316,
            WorkloadClass::Recovery => {
                spec.overlay_near = 216;
                spec.recovery_at = Some(60);
                spec.horizon = 200;
            }
            WorkloadClass::MixedLod => {}
            WorkloadClass::ChangingPresence => {
                spec.lod_transitions = vec![(75u64, [LodLevel::EventDriven; REGION_COUNT])];
            }
        }
        let (_first, _second, deterministic) = run_cell_deterministic(&spec);
        assert!(
            deterministic,
            "class {} must be double-run identical",
            class.name()
        );
    }
}

/// F6 — the horizon derivation anchors per population: the
/// pure-synthetic population's drain is exactly 50 ticks (backlog
/// 1,000 at 20/tick) → horizon 500; the 1k-mixed population's own
/// drain (base ≈ 84/tick continuing after the arc) is measured on
/// ITS arc and derives its own horizon — never one global drain time.
#[test]
fn horizon_derivation_from_measured_drain() {
    let mut synthetic = CellSpec::base(WorkloadClass::Recovery, 0, lod_scenarios()[0]);
    synthetic.budget = 20;
    synthetic.reserve = 8;
    synthetic.overlay_near = 30;
    synthetic.recovery_at = Some(100);
    synthetic.horizon = 300;
    let synthetic_report = run_cell(&synthetic);
    let synthetic_drain =
        measured_drain_tick(&synthetic_report.windows).expect("the synthetic arc drains");
    assert_eq!(synthetic_drain, 150);
    assert_eq!(derived_horizon(synthetic_drain - 100), 500);

    let mut mixed = CellSpec::base(WorkloadClass::Recovery, 1_000, lod_scenarios()[1]);
    mixed.overlay_near = 216;
    mixed.recovery_at = Some(100);
    mixed.horizon = 400;
    let mixed_report = run_cell(&mixed);
    let mixed_drain = measured_drain_tick(&mixed_report.windows).expect("the 1k arc drains");
    assert!(
        mixed_drain > 100,
        "the 1k drain completes after the arc (measured: {mixed_drain})"
    );
    assert!(
        derived_horizon(mixed_drain) >= 10 * (mixed_drain - 100),
        "the derived horizon is at least 10× this population's measured drain"
    );
    assert!(mixed_report.conservation_holds);
}
