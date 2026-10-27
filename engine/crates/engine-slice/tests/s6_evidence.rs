//! S6-C1 instrumentation integration gates (Stage 6 Checkpoint 1, the
//! owner GO of 2026-10-27): the evidence collector composed into the
//! real headless demonstration — the logical-fingerprint equivalence
//! gate (instrumentation must never change authoritative state), the
//! error-path gate (a failing evidence writer must never bypass the
//! accepted graceful close), the package-integrity gate (counts,
//! files, manifest), and the offscreen graphical smoke (pod-only —
//! the frame-sample producers under the software Vulkan ICD).

use std::path::PathBuf;

use engine_slice::persist_demo::{DemoOptions, run_persist_demo, run_persist_demo_headless};
use engine_slice::region::{TransitionKind, TransitionScope};
use engine_slice::soak_evidence::{
    EvidenceConfig, EvidenceRecord, RegionSample, decode_line, verify_package,
};

/// A unique test campaign root (removed on drop — best-effort).
struct TestDir {
    root: PathBuf,
}

impl TestDir {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "s7-s6c1-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        TestDir { root }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn options(dir: &TestDir, ticks: u64, action_at: Option<u64>) -> DemoOptions {
    DemoOptions {
        campaign_dir: dir.root.clone(),
        fps: 60,
        present: engine_renderer::PresentModePreference::Auto,
        frames: None,
        overlay: true,
        validation: engine_renderer::ValidationPreference::Auto,
        headless: true,
        headless_ticks: ticks,
        action_at,
        poison_at_tick: None,
        evidence: None,
        evidence_fail_writes: false,
    }
}

/// Instrumentation enabled must not change the run's logical state:
/// the same headless scenario (400 ticks, the action at 240), two
/// fresh campaigns, one with evidence collection — identical closed
/// fingerprints, ticks, conservation inputs, and durable outcome
/// witnesses, plus a complete evidence verdict.
#[test]
fn instrumentation_enabled_preserves_the_logical_fingerprints() {
    let plain_dir = TestDir::new("equivalence-plain");
    let evidence_dir = TestDir::new("equivalence-evidence");
    let plain = run_persist_demo_headless(options(&plain_dir, 400, Some(240)))
        .expect("the plain run completes");
    let mut instrumented_options = options(&evidence_dir, 400, Some(240));
    instrumented_options.evidence = Some(EvidenceConfig::new(
        evidence_dir.root.join("evidence"),
        "s6c1-equivalence",
    ));
    let instrumented =
        run_persist_demo_headless(instrumented_options).expect("the instrumented run completes");

    assert_eq!(plain.closed_fingerprint, instrumented.closed_fingerprint);
    assert_eq!(plain.closed_at_tick, instrumented.closed_at_tick);
    assert_eq!(plain.closed_ticks_due, instrumented.closed_ticks_due);
    assert_eq!(plain.closed_outstanding, instrumented.closed_outstanding);
    assert_eq!(plain.closed_region, instrumented.closed_region);
    assert_eq!(
        plain.action.as_ref().map(|record| record.describe()),
        instrumented.action.as_ref().map(|record| record.describe())
    );
    assert!(plain.close.failure.is_none());
    assert!(instrumented.close.failure.is_none());

    let outcome = instrumented.evidence.as_ref().expect("the verdict");
    assert!(outcome.is_complete(), "{outcome:?}");
    assert!(outcome.total_samples() >= 400, "{outcome:?}");
}

/// A failing evidence writer must never touch the close's outcome: a
/// clean campaign whose writer fails deterministically (the
/// fail-writes seam) still closes COMPLETE — the application error is
/// absent, the evidence verdict alone fails, honestly counted.
#[test]
fn a_failing_evidence_writer_leaves_a_clean_close_untouched() {
    let dir = TestDir::new("writer-failure-clean");
    let mut failing = options(&dir, 300, Some(150));
    failing.evidence = Some(EvidenceConfig::new(
        dir.root.join("evidence"),
        "s6c1-writer-failure-clean",
    ));
    failing.evidence_fail_writes = true;
    let report = run_persist_demo_headless(failing).expect("the run finalizes");

    assert!(report.application_failure.is_none());
    assert!(report.close.failure.is_none(), "{:?}", report.close.failure);
    assert!(report.close.lifecycle_checkpoint);
    assert!(report.close.required_settled);

    let outcome = report.evidence.as_ref().expect("the verdict");
    assert!(!outcome.is_complete(), "{outcome:?}");
    assert!(outcome.failure.is_some(), "{outcome:?}");
    let ticks = outcome
        .streams
        .iter()
        .find(|row| row.name == "ticks")
        .expect("the ticks stream");
    assert!(ticks.produced > 150, "{ticks:?}");
    assert_eq!(ticks.produced, ticks.write_failed, "{ticks:?}");
}

/// The error-path composition: a failing evidence writer rides the
/// accepted application-error finalization (the poison seam) — the
/// single finalization site still runs, the original error and the
/// poisoned worker's honest close failure are both reported, and the
/// evidence failure is counted alongside, never silently and never
/// bypassing the close.
#[test]
fn a_failing_evidence_writer_rides_the_error_path_finalization() {
    let dir = TestDir::new("writer-failure-poisoned");
    let mut failing = options(&dir, 300, Some(120));
    failing.poison_at_tick = Some(120);
    failing.evidence = Some(EvidenceConfig::new(
        dir.root.join("evidence"),
        "s6c1-writer-failure-poisoned",
    ));
    failing.evidence_fail_writes = true;
    let report = run_persist_demo_headless(failing).expect("the run finalizes");

    let failure = report
        .application_failure
        .as_ref()
        .expect("the application failure preserved");
    assert!(failure.contains("tick 120 refused"), "{failure}");
    // The accepted poisoned-worker semantics (the Checkpoint 1
    // regression's assertions): the close FAILED honestly — reported
    // as a failure, never as success — which is itself the proof the
    // finalization ran and was not bypassed by the evidence failure.
    assert!(report.close.failure.is_some());
    assert!(!report.close.required_settled);

    let outcome = report.evidence.as_ref().expect("the verdict");
    assert!(!outcome.is_complete(), "{outcome:?}");
    assert!(outcome.failure.is_some(), "{outcome:?}");
    let ticks = outcome
        .streams
        .iter()
        .find(|row| row.name == "ticks")
        .expect("the ticks stream");
    // Deterministic: ticks 0-119 executed (the poison refuses tick
    // 120), so exactly 120 tick samples — every one counted as a
    // failed write, none silently lost.
    assert_eq!(ticks.produced, 120, "{ticks:?}");
    assert_eq!(ticks.produced, ticks.write_failed, "{ticks:?}");
}

/// A clean instrumented run writes a complete package: the manifest
/// records every stream's counts, the JSONL files exist and decode,
/// and the tick stream's first record is a well-formed tick.
#[test]
fn a_clean_run_writes_a_complete_evidence_package() {
    let dir = TestDir::new("package");
    let mut instrumented = options(&dir, 300, Some(150));
    let evidence_dir = dir.root.join("evidence");
    instrumented.evidence = Some(EvidenceConfig::new(&evidence_dir, "s6c1-package"));
    let report = run_persist_demo_headless(instrumented).expect("the run completes");
    assert!(report.close.failure.is_none());
    let outcome = report.evidence.as_ref().expect("the verdict");
    assert!(outcome.is_complete(), "{outcome:?}");

    let manifest = std::fs::read_to_string(evidence_dir.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"schema\":\"s6-evidence/2\""));
    assert!(manifest.contains("\"scenario\":\"s6c1-package\""));
    assert!(manifest.contains("\"complete\":true"));
    assert!(manifest.contains("\"failure\":null"));
    assert!(manifest.contains("\"dir_synced\":"));
    let verification = verify_package(&evidence_dir).unwrap();
    assert!(
        verification.verified_complete(),
        "{:?}",
        verification.problems
    );

    let ticks = std::fs::read_to_string(evidence_dir.join("ticks.jsonl")).unwrap();
    let lines: Vec<&str> = ticks.lines().collect();
    assert_eq!(lines.len(), 300);
    for line in &lines {
        assert!(decode_line(line).is_ok(), "undecodable tick line: {line}");
    }
    for stream in ["frames", "persistence", "regions", "memory"] {
        assert!(
            evidence_dir.join(format!("{stream}.jsonl")).is_file(),
            "the {stream} stream file exists"
        );
    }
    let persistence = std::fs::read_to_string(evidence_dir.join("persistence.jsonl")).unwrap();
    let kinds: Vec<&str> = persistence
        .lines()
        .filter_map(|line| decode_line(line).ok())
        .filter_map(|record| match record {
            engine_slice::soak_evidence::EvidenceRecord::Persistence(sample) => {
                Some(match sample.kind {
                    engine_slice::soak_evidence::PersistenceKind::Recovery => "recovery",
                    engine_slice::soak_evidence::PersistenceKind::Close => "close",
                    engine_slice::soak_evidence::PersistenceKind::Capture => "capture",
                    engine_slice::soak_evidence::PersistenceKind::Admission => "admission",
                    engine_slice::soak_evidence::PersistenceKind::Completion => "completion",
                    engine_slice::soak_evidence::PersistenceKind::Confirmation => "confirmation",
                    engine_slice::soak_evidence::PersistenceKind::Busy => "busy",
                })
            }
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&"recovery"), "{kinds:?}");
    assert!(kinds.contains(&"close"), "{kinds:?}");
    assert!(kinds.contains(&"confirmation"), "{kinds:?}");
}

/// The region stream's per-transition records (S6-C1 HOLD-4): every
/// entry carries its own duration, kind, and boundary scope — the
/// excursion's target shows the FULL orchestrated crossing pair (a
/// deactivation then a reactivation, never the install phase alone),
/// every duration is present (never a substituted zero), every
/// recorded transition applied (a clean run), and the independent
/// verification accepts the package.
#[test]
fn the_excursion_crossings_carry_full_orchestrated_per_transition_durations() {
    let dir = TestDir::new("excursion-crossings");
    let mut run_options = options(&dir, 300, Some(120));
    run_options.evidence = Some(EvidenceConfig::new(
        dir.root.join("evidence"),
        "s6c1-excursion-crossings",
    ));
    let report = run_persist_demo_headless(run_options).expect("the run completes");
    assert!(report.close.failure.is_none());
    let outcome = report.evidence.as_ref().expect("the verdict");
    assert!(outcome.is_complete(), "{outcome:?}");

    let evidence_dir = dir.root.join("evidence");
    let mut entries = Vec::new();
    for line in std::fs::read_to_string(evidence_dir.join("regions.jsonl"))
        .unwrap()
        .lines()
    {
        match decode_line(line).expect("decodable region line") {
            EvidenceRecord::Region(sample) => entries.push(sample),
            other => panic!("a non-region record in the regions stream: {other:?}"),
        }
    }
    assert!(!entries.is_empty());

    // The excursion's target pair: the FULL six-phase orchestrated
    // deactivation, then the durable reactivation — individually
    // timed.
    let target = u64::from(report.action.as_ref().expect("the action").region as u16);
    let deactivations: Vec<&RegionSample> = entries
        .iter()
        .filter(|entry| {
            entry.region == target
                && entry.kind == TransitionKind::Deactivation
                && entry.scope == TransitionScope::OrchestratedCrossing
        })
        .collect();
    let activations: Vec<&RegionSample> = entries
        .iter()
        .filter(|entry| {
            entry.region == target
                && entry.kind == TransitionKind::Activation
                && entry.scope == TransitionScope::OrchestratedCrossing
        })
        .collect();
    assert_eq!(deactivations.len(), 1, "{entries:?}");
    assert_eq!(activations.len(), 1, "{entries:?}");

    // Every record: a present duration, an applied transition, and
    // the founding/reclassify work honestly labeled with the
    // director-legacy scope (both scopes present in a real run).
    for entry in &entries {
        assert!(
            entry.duration_ms.is_some(),
            "no substituted zero: {entry:?}"
        );
        assert!(
            entry.applied,
            "a clean run applies every recorded transition: {entry:?}"
        );
    }
    assert!(
        entries
            .iter()
            .any(|entry| entry.scope == TransitionScope::DirectorTransition),
        "the founding/reclassify work carries the director-legacy scope: {entries:?}"
    );

    // The per-tick cross-check: every TickReport transitioned region
    // has exactly one region record (plus the founding entries
    // emitted before the first tick).
    let mut transitioned_total = 0u64;
    for line in std::fs::read_to_string(evidence_dir.join("ticks.jsonl"))
        .unwrap()
        .lines()
    {
        if let EvidenceRecord::Tick(sample) = decode_line(line).unwrap() {
            transitioned_total += sample.transitioned_regions.len() as u64;
        }
    }
    assert!(
        entries.len() as u64 >= transitioned_total,
        "every reported transition carries a record (plus the founding entries)"
    );

    let verification = verify_package(&evidence_dir).unwrap();
    assert!(
        verification.verified_complete(),
        "{:?}",
        verification.problems
    );
}

/// The director's own transition path (S6-C1 HOLD-4): the
/// pipeline-free legacy transitions log individually timed entries
/// with the DirectorTransition scope — one entry per level change,
/// exact count, no overflow, never a tick aggregate.
#[test]
fn director_legacy_transitions_log_individual_durations_and_scope() {
    let (mut spine, mut director) = engine_slice::persist_demo::demo_scenario(
        &engine_slice::sim::SimConfig::default(),
        &[engine_slice::persist_demo::DEMO_PRESENCE],
    )
    .expect("the scenario builds");
    // Clear the founding demotions (the loops emit those separately);
    // this test drives the legacy path directly.
    let _ = director.take_transition_entries();

    let far = engine_core::PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    };
    let mut expected_entries = 0usize;
    for _ in 0..40 {
        let before = director.levels();
        director.apply(&mut spine, std::slice::from_ref(&far));
        let after = director.levels();
        expected_entries += (0..8usize)
            .filter(|region| before[*region] != after[*region])
            .count();
        spine.run_tick();
    }
    let entries = director.take_transition_entries();
    assert!(!entries.is_empty());
    assert_eq!(
        entries.len(),
        expected_entries,
        "one entry per level change — never a tick aggregate"
    );
    for entry in &entries {
        assert_eq!(entry.scope, TransitionScope::DirectorTransition);
        assert!(
            entry.duration_ms.is_some(),
            "no substituted zero: {entry:?}"
        );
        assert!(
            entry.applied,
            "the far-presence demotions all apply: {entry:?}"
        );
    }
    assert_eq!(director.transition_log_overflow(), 0);
}

/// The frame-sample producers under the software Vulkan ICD — the
/// pod-only graphical smoke (the CI runners without a Vulkan loader
/// never run this). Run explicitly with the offscreen driver:
///
/// ```sh
/// SDL_VIDEODRIVER=offscreen cargo test -p engine-slice --test s6_evidence -- \
///     --ignored graphical_offscreen_evidence_smoke --nocapture
/// ```
#[test]
#[ignore = "requires SDL_VIDEODRIVER=offscreen and a software Vulkan ICD (lavapipe); pod-only"]
fn graphical_offscreen_evidence_smoke() {
    if std::env::var("SDL_VIDEODRIVER").is_err() {
        panic!(
            "SDL_VIDEODRIVER is not set — run this smoke under the offscreen driver \
             with a software Vulkan ICD (see the test's doc comment)"
        );
    }
    let dir = TestDir::new("graphical-smoke");
    let mut graphical = options(&dir, 0, None);
    graphical.headless = false;
    graphical.frames = Some(10);
    graphical.fps = 0;
    graphical.evidence = Some(EvidenceConfig::new(
        dir.root.join("evidence"),
        "s6c1-graphical-smoke",
    ));
    // A clean run completes (the single finalization site's combined
    // outcome includes the evidence verdict — a failed package fails
    // the run).
    run_persist_demo(graphical).expect("the graphical smoke completes");
    let evidence_dir = dir.root.join("evidence");
    let frames = std::fs::read_to_string(evidence_dir.join("frames.jsonl")).unwrap();
    assert!(
        frames.lines().count() >= 10,
        "expected at least 10 frame samples, got {}",
        frames.lines().count()
    );
    for line in frames.lines() {
        assert!(decode_line(line).is_ok(), "undecodable frame line: {line}");
    }
}
