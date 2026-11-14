//! The P2 sustained-load CI regressions (Cycle 1, SEAT B): the
//! deterministic, logical subset of the amended acceptance matrix —
//! no wall-clock thresholds in CI, no crash-test-seams dependency,
//! platform-uniform (Linux-only memory sampling stays report-side).
//!
//! This file grows to the six amended regressions; it opens with the
//! double-run determinism smoke at small scale (methodology §8.4's
//! CI form).

use engine_slice::sustained_load::{
    CellSpec, WorkloadClass, lod_scenarios, run_cell_deterministic,
};

/// The double-run determinism proof at CI scale: two independent
/// constructions of the same cell must produce byte-identical trace
/// digests and world fingerprints (the §8.4 proof; conservation is
/// asserted inside the runner itself).
#[test]
fn double_run_determinism_smoke() {
    let scenario = lod_scenarios()[1];
    let mut spec = CellSpec::base(WorkloadClass::MixedLod, 100, scenario);
    spec.horizon = 100;
    let (first, _second, deterministic) = run_cell_deterministic(&spec);
    assert!(deterministic, "the double-run must be byte-identical");
    assert!(
        first.executed > 0,
        "the mixed cell executes real chain work"
    );
    assert!(
        first.conservation_holds,
        "conservation holds across the run"
    );
}
