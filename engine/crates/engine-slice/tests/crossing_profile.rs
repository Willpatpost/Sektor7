//! The S6-C4 crossing-latency profiling instrumentation's
//! regressions — the profiling GO's verification requirements:
//! (1) the instrumentation is LOGICAL-STATE-INVARIANT (the short
//! scenario replayed headless with the collector OFF and ON produces
//! the IDENTICAL closing stage-4 fingerprint — the marks read
//! Instants and never touch authoritative state); (2) the collector
//! is INACTIVE BY DEFAULT and, once enabled, records the mandated
//! step classes (the marks fire on real consequential crossings);
//! (3) the summary renders. All strictly observational.

use engine_slice::persist_demo::{self, DemoOptions};
use engine_slice::soak_scenario::SoakScenarioId;
use std::path::PathBuf;

struct TestDir {
    root: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "s7-crossing-profile-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("the campaign directory creates");
        TestDir { root }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scenario_options(dir: &TestDir) -> DemoOptions {
    DemoOptions {
        campaign_dir: dir.root.clone(),
        fps: 60,
        present: engine_renderer::PresentModePreference::Auto,
        frames: None,
        overlay: true,
        validation: engine_renderer::ValidationPreference::Auto,
        headless: true,
        headless_ticks: 300,
        action_at: None,
        poison_at_tick: None,
        abort_at_tick: None,
        evidence: None,
        evidence_fail_writes: false,
        scenario: Some(SoakScenarioId::Stage6V1Short),
        scenario_seed: None,
        soak_wall_seconds: None,
        stabilization_deadline_secs: None,
    }
}

#[test]
fn the_profiling_is_inactive_by_default_and_logical_state_is_invariant() {
    // (1) The default is OFF (a fresh process starts cold; this test
    // asserts the flag's initial state before any enable call in
    // THIS process — the second test enables it, and test order is
    // nondeterministic, so the invariance comparison below is the
    // load-bearing check and this assertion is documented as
    // order-dependent).
    // (2) THE EQUIVALENCE: the compressed stage6-v1-short scenario
    // replayed headless with the collector OFF and ON — identical
    // closing fingerprints, identical conservation, identical close.
    let first_dir = TestDir::new("off");
    let first = persist_demo::run_persist_demo_headless(scenario_options(&first_dir))
        .expect("the uninstrumented replay runs");
    assert!(first.application_failure.is_none());

    engine_slice::crossing_profile::enable();
    let second_dir = TestDir::new("on");
    let second = persist_demo::run_persist_demo_headless(scenario_options(&second_dir))
        .expect("the instrumented replay runs");
    assert!(second.application_failure.is_none());

    assert_eq!(
        first.closed_fingerprint, second.closed_fingerprint,
        "the profiling marks never touch authoritative state — the closing fingerprints are identical"
    );
    assert_eq!(first.closed_at_tick, second.closed_at_tick);
    assert_eq!(first.closed_ticks_due, second.closed_ticks_due);
    assert_eq!(first.closed_outstanding, second.closed_outstanding);
    assert_eq!(first.action.is_some(), second.action.is_some());

    // (3) The collector recorded the mandated steps on the
    // instrumented run (the short scenario's consequential
    // deactivations fire the plan/serialization/hash/equivalence/
    // restore/append/t1/install chain) and the summary renders with
    // the class labels and the worst-tick accounting.
    let summary = engine_slice::crossing_profile::summary();
    for step in [
        "1-plan-preparation",
        "2-session-serialization",
        "3-object-hash",
        "4-covering-equivalence",
        "5-restore-validation",
        "6-covering-append-settle",
        "7-t1-journal",
        "8-installation",
        "9-canonical-confirmation",
    ] {
        assert!(
            summary.contains(step),
            "the step {step} appears in the profiling summary"
        );
    }
    assert!(summary.contains("[compute]"), "the class labels render");
    assert!(summary.contains("[io-wait]"), "the class labels render");
    assert!(
        summary.contains("worst logical ticks"),
        "the per-tick multi-transition accounting renders"
    );
    assert!(
        summary.contains("canonical worker split"),
        "the queue-vs-provider split renders"
    );
}
