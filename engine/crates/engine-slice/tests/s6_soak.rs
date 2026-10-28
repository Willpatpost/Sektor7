//! S6-C3 automated integrated soak runner verification (Stage 6
//! Checkpoint 3, the owner GO of 2026-10-27): the tick-indexed
//! scenario, the strict namespaced CLI, the corrected time-debt
//! termination protocol, checkpoint-scale identity accounting, and
//! the fresh-process recovery — over the REAL orchestrated session
//! and the REAL binary.
//!
//! **The discipline:**
//!
//! *Tick-indexed inputs* — the same scripted scenario driven under
//! four different frame/catch-up patterns (one tick per call, the
//! real-time cadence, 4-tick catch-up batches, and one deep stall
//! followed by the no-new-time drain) must produce IDENTICAL logical
//! state: the same accepted identities, the same executions, the same
//! checkpoint fingerprints, the same durable outcome. Scripted inputs
//! are applied per LOGICAL tick inside `run_ticks` — never per
//! rendered frame — so cadence cannot change logical behavior.
//!
//! *Recovery and failure honesty* — the closed campaign recovers in a
//! FRESH PROCESS (the real binary) with the exact durable state; an
//! externally terminated run recovers from whatever durable state
//! exists (its close status honestly unknown); an unable-to-drain
//! debt is reported, never discarded; a failed evidence package fails
//! the run visibly.
//!
//! *Checkpoint accounting* — the O(1) whole-campaign conservation
//! identity at every controlled boundary; the dormant-held cohort
//! observed mid-hold; the sustained-overload backlog observed growing
//! and then draining; the full identity-level zero-silent-drop
//! verdict at finalization through the FULL ledger.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use engine_core::{EventClass, EventSpec, Field, Schedule, Tick};
use engine_persist::{AuthorityMode, CanonicalMedium, RevisionRecord, T1Journal};
use engine_slice::orchestration::OrchestratedSession;
use engine_slice::persist_demo::{
    DemoOptions, PersistDemo, SoakDrain, demo_scenario, publish_soak_summary, region_credit_sum,
    run_persist_demo, run_persist_demo_headless, soak_drain_gate,
    soak_evidence_verification_failure, soak_validation_gate,
};
use engine_slice::sim::SimConfig;
use engine_slice::soak_evidence::{
    EvidenceConfig, EvidenceError, EvidenceRecord, PackageVerification, PersistenceKind,
    VerifiedStream, verify_package,
};
use engine_slice::soak_scenario::{
    STAGE6_SCENARIO_SEED, SoakOptions, SoakScenario, SoakScenarioId, coverage_gate_failure,
};
use engine_slice::store::{SessionStore, StoreLocation};

/// The frame/catch-up patterns the same scenario must survive
/// identically (§7B — frame-cadence independence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pattern {
    /// The headless replay: one free tick per call (no accumulator).
    HeadlessPerTick,
    /// The real-time cadence: 100 ms per advance — exactly one tick
    /// becomes due per call at the 10 Hz campaign rate.
    RealTime,
    /// A 4-tick catch-up batch per frame (400 ms per advance).
    CatchUp4,
    /// One deep stall — 30 seconds of wall time arrive at once, the
    /// bounded per-frame catch-up retains the debt — then the
    /// no-new-time drain (`advance(Duration::ZERO)`) settles it.
    StallThenDrain,
}

/// A unique campaign/evidence root (removed on drop — best-effort).
struct TestDir {
    root: PathBuf,
}

impl TestDir {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "s7-s6c3-{tag}-{}",
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

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| text.to_string()).collect()
}

/// Headless scenario options (the accelerated replay).
fn scenario_options(dir: &TestDir, scenario: SoakScenarioId, seed: Option<u64>) -> DemoOptions {
    DemoOptions {
        campaign_dir: dir.root.clone(),
        fps: 60,
        present: engine_renderer::PresentModePreference::Auto,
        frames: None,
        overlay: true,
        validation: engine_renderer::ValidationPreference::Auto,
        headless: true,
        headless_ticks: scenario.horizon(),
        action_at: None,
        poison_at_tick: None,
        evidence: None,
        evidence_fail_writes: false,
        scenario: Some(scenario),
        scenario_seed: seed,
        soak_wall_seconds: None,
        stabilization_deadline_secs: None,
    }
}

/// One driven run's comparable logical outcome — everything read
/// BEFORE the finalization consumes the session.
struct DriveOutcome {
    accepted_seqs: Vec<u64>,
    executed_seqs: Vec<u64>,
    checkpoint_fingerprints: Vec<Option<String>>,
    terminal_fingerprint: String,
    region_credits: Vec<u64>,
    scenario_verdict: bool,
    cohorts_drained: bool,
    actions: usize,
    close_final_revision: Option<String>,
    close_failure: bool,
}

/// Drives the compressed scenario under the given pattern over a
/// FRESH campaign and reads the comparable outcome.
fn drive(tag: &str, pattern: Pattern, seed: u64) -> DriveOutcome {
    let cleaner = TestDir::new(tag);
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&cleaner.root, SimConfig::default()).expect("the open");
    demo.attach_scenario(SoakScenario::new(
        SoakScenarioId::Stage6V1Short,
        seed,
        demo.session().director().config().centers,
    ));
    let ticks = SoakScenarioId::Stage6V1Short.horizon();
    match pattern {
        Pattern::HeadlessPerTick => {
            for _ in 0..ticks {
                demo.run_headless(1).expect("the tick runs");
            }
        }
        Pattern::RealTime => {
            for _ in 0..ticks {
                demo.advance(Duration::from_millis(100))
                    .expect("the advance runs");
            }
        }
        Pattern::CatchUp4 => {
            for _ in 0..ticks / 4 {
                demo.advance(Duration::from_millis(400))
                    .expect("the advance runs");
            }
        }
        Pattern::StallThenDrain => {
            // The whole scenario's wall time arrives in ONE frame: 300
            // ticks become due, the bounded catch-up executes 8, and
            // the retained debt drains through the no-new-time
            // primitive — every scripted input still applies per
            // logical tick inside the batches.
            demo.advance(Duration::from_millis(ticks * 100))
                .expect("the stall advance");
            while demo.outstanding_ticks() > 0 {
                demo.advance(Duration::ZERO).expect("the drain advance");
            }
        }
    }
    assert_eq!(
        demo.session().clock().as_u64(),
        ticks,
        "every pattern executes exactly the horizon's ticks"
    );
    demo.wait_current();
    let scenario = demo.scenario().expect("the scenario ran");
    let terminal = scenario.terminal_accounting(demo.session());
    let publications = engine_slice::soak_scenario::PublicationTerminal {
        pending: demo.session().pending_publications(),
        tracked_pending: matches!(
            demo.publication_state(),
            Some(engine_slice::persist_demo::PublicationState::Pending { .. })
        ),
    };
    let report = scenario.run_report(terminal, publications);
    let accepted_seqs = {
        let mut seqs: Vec<u64> = scenario
            .registered()
            .iter()
            .map(|entry| entry.id.seq)
            .collect();
        seqs.sort_unstable();
        seqs
    };
    let executed_seqs = {
        let mut seqs: Vec<u64> = scenario
            .registered()
            .iter()
            .filter(|entry| demo.session().spine().is_executed(entry.id))
            .map(|entry| entry.id.seq)
            .collect();
        seqs.sort_unstable();
        seqs
    };
    let checkpoint_fingerprints = report
        .checkpoints
        .iter()
        .map(|checkpoint| checkpoint.fingerprint.clone())
        .collect();
    let terminal_fingerprint = engine_slice::region::stage4_state_fingerprint_hex(
        demo.session().spine(),
        demo.session().director(),
    );
    let region_credits = (0..8)
        .map(|region| region_credit_sum(demo.session().spine().world(), region))
        .collect();
    let outcome = DriveOutcome {
        accepted_seqs,
        executed_seqs,
        checkpoint_fingerprints,
        terminal_fingerprint,
        region_credits,
        scenario_verdict: report.verdict_ok(),
        cohorts_drained: report.cohorts_drained(),
        actions: report.actions.len(),
        close_final_revision: None,
        close_failure: false,
    };
    let finalization = demo.finalize(None);
    DriveOutcome {
        close_final_revision: finalization
            .close_summary()
            .final_revision
            .map(|revision| revision.to_string()),
        close_failure: finalization.close_summary().failure.is_some(),
        ..outcome
    }
}

/// §3/§10: the strict namespaced CLI — every invalid value and
/// incompatible combination is a VISIBLE error (never a silent
/// default, never a silent clamp).
#[test]
fn the_scenario_cli_rejects_invalid_values_and_combinations() {
    // Per-flag value validity (parse).
    assert!(SoakOptions::parse(&args(&["--soak-wall-seconds"])).is_err());
    assert!(SoakOptions::parse(&args(&["--soak-wall-seconds", "abc"])).is_err());
    assert!(SoakOptions::parse(&args(&["--soak-wall-seconds", "0"])).is_err());
    assert!(SoakOptions::parse(&args(&["--soak-wall-seconds", "1000000"])).is_err());
    assert!(SoakOptions::parse(&args(&["--stabilization-deadline-secs", "0"])).is_err());
    assert!(SoakOptions::parse(&args(&["--scenario", "bogus"])).is_err());
    assert!(SoakOptions::parse(&args(&["--scenario"])).is_err());
    assert!(SoakOptions::parse(&args(&["--scenario-seed", "xyz"])).is_err());
    assert!(SoakOptions::parse(&args(&["--evidence-dir"])).is_err());
    let valid = SoakOptions::parse(&args(&[
        "--soak-wall-seconds",
        "900",
        "--scenario",
        "stage6-v1",
        "--evidence-dir",
        "/tmp/s7-unused",
        "--stabilization-deadline-secs",
        "60",
        "--scenario-seed",
        "7",
    ]))
    .expect("the valid combination parses");
    assert_eq!(valid.wall_seconds, Some(900));
    assert_eq!(valid.scenario, Some(SoakScenarioId::Stage6V1));
    assert_eq!(valid.scenario_seed, Some(7));
    assert_eq!(valid.stabilization_deadline_secs, Some(60));

    // The relational rules (validate) against the mode context.
    let soak = SoakOptions {
        wall_seconds: Some(900),
        stabilization_deadline_secs: None,
        scenario: Some(SoakScenarioId::Stage6V1),
        scenario_seed: None,
        evidence_dir: Some(PathBuf::from("/tmp/s7-unused")),
    };
    assert!(soak.validate(false, None, None, false).is_ok());
    assert!(
        soak.validate(true, None, None, false).is_err(),
        "headless is rejected"
    );
    assert!(
        soak.validate(false, Some(60), None, false).is_err(),
        "frames are rejected"
    );
    assert!(
        soak.validate(false, None, None, true).is_err(),
        "ticks are rejected"
    );
    let scenario_only = SoakOptions {
        scenario: Some(SoakScenarioId::Stage6V1Short),
        ..SoakOptions::default()
    };
    assert!(scenario_only.validate(true, None, None, false).is_ok());
    assert!(
        scenario_only.validate(false, None, None, false).is_err(),
        "a graphical scenario requires the wall window"
    );
    assert!(
        scenario_only.validate(true, None, Some(5), false).is_err(),
        "the scenario owns the action schedule"
    );
    assert!(
        SoakOptions::default()
            .validate(true, None, None, false)
            .is_ok(),
        "no soak flags at all is the unchanged default"
    );
}

/// §7A/§7B: the same scenario, four different frame/catch-up
/// patterns — identical accepted identities, executions, checkpoint
/// fingerprints, terminal state, durable outcome. Scripted inputs are
/// indexed by LOGICAL ticks, so cadence cannot change logical
/// behavior (the camera's frame-sampled state is never the oracle).
#[test]
fn scripted_inputs_apply_per_logical_tick_across_catch_up_patterns() {
    let headless = drive(
        "cadence-headless",
        Pattern::HeadlessPerTick,
        STAGE6_SCENARIO_SEED,
    );
    let realtime = drive("cadence-realtime", Pattern::RealTime, STAGE6_SCENARIO_SEED);
    let catchup = drive("cadence-catchup", Pattern::CatchUp4, STAGE6_SCENARIO_SEED);
    let stalled = drive(
        "cadence-stall",
        Pattern::StallThenDrain,
        STAGE6_SCENARIO_SEED,
    );
    for (label, other) in [
        ("real-time cadence", &realtime),
        ("4-tick catch-up batches", &catchup),
        ("the stalled frame + no-new-time drain", &stalled),
    ] {
        assert_eq!(
            headless.accepted_seqs, other.accepted_seqs,
            "the accepted identity set is cadence-independent ({label})"
        );
        assert_eq!(
            headless.executed_seqs, other.executed_seqs,
            "the executed identity set is cadence-independent ({label})"
        );
        assert_eq!(
            headless.checkpoint_fingerprints, other.checkpoint_fingerprints,
            "the checkpoint fingerprint series is cadence-independent ({label})"
        );
        assert_eq!(
            headless.terminal_fingerprint, other.terminal_fingerprint,
            "the terminal fingerprint is cadence-independent ({label})"
        );
        assert_eq!(
            headless.region_credits, other.region_credits,
            "the consequential credits are cadence-independent ({label})"
        );
        assert_eq!(
            headless.close_final_revision, other.close_final_revision,
            "the durable final revision is cadence-independent ({label})"
        );
        assert_eq!(
            headless.actions, other.actions,
            "the scripted action count is cadence-independent ({label})"
        );
    }
    assert_eq!(
        headless.actions, 1,
        "the short profile's single action cycle"
    );
    for outcome in [&headless, &realtime, &catchup, &stalled] {
        assert!(outcome.scenario_verdict, "the scenario verdict holds");
        assert!(outcome.cohorts_drained, "the finite cohorts drained");
        assert!(!outcome.close_failure, "the close completed");
    }
}

/// §4: the seed is a real part of the schedule identity — a different
/// seed changes the deterministic due-offset jitter (and therefore
/// the state), while the schedule's shape (the registered counts) is
/// version-fixed.
#[test]
fn the_seed_parameterizes_the_schedule() {
    let canonical = drive(
        "seed-canonical",
        Pattern::HeadlessPerTick,
        STAGE6_SCENARIO_SEED,
    );
    let reseeded = drive(
        "seed-other",
        Pattern::HeadlessPerTick,
        STAGE6_SCENARIO_SEED + 1,
    );
    assert_eq!(
        canonical.accepted_seqs.len(),
        reseeded.accepted_seqs.len(),
        "the schedule's shape is version-fixed"
    );
    assert_ne!(
        canonical.terminal_fingerprint, reseeded.terminal_fingerprint,
        "the seed changes the deterministic schedule (the due-offset jitter)"
    );
    assert!(reseeded.scenario_verdict, "the reseeded run still verifies");
}

/// §8: the checkpoint-scale accounting — the conservation identity at
/// every boundary, the dormant-held cohort observed MID-HOLD, the
/// sustained-overload backlog observed growing and then draining, and
/// the full identity-level zero-silent-drop verdict at finalization.
#[test]
fn checkpoints_verify_conservation_the_held_path_and_the_overload_backlog() {
    let cleaner = TestDir::new("checkpoints");
    let report = run_persist_demo_headless(scenario_options(
        &cleaner,
        SoakScenarioId::Stage6V1Short,
        None,
    ))
    .expect("the scenario run completes");
    let scenario = report.scenario.as_ref().expect("the scenario report");
    assert!(scenario.checkpoints.iter().all(|cp| cp.conservation));
    assert!(scenario.terminal.conservation);
    assert!(scenario.terminal.history_full);
    assert_eq!(scenario.terminal.identity_evidence, "full-ledger");
    // The checkpoint series is ordered by the boundary ticks: the
    // recorded `scenario_tick` is the count of scripted ticks
    // CONSUMED at the boundary (the boundary tick itself included).
    // The compressed tour's full-hop lookahead releases the held
    // cohort when the tour reaches the region BEFORE the target —
    // the deterministic hold window is ticks 231–~238, so the 234
    // boundary observes the hold and the 240 boundary the release.
    let held_checkpoint = &scenario.checkpoints[5]; // the 234 boundary — mid-hold
    let held_cohort = held_checkpoint
        .cohorts
        .iter()
        .find(|cohort| cohort.cohort == engine_slice::soak_scenario::HELD_COHORT)
        .expect("the held cohort is accounted");
    assert!(
        held_cohort.held > 0,
        "the dormant-target cohort is observed MID-HOLD (the held path): {held_cohort:?}"
    );
    let released_checkpoint = &scenario.checkpoints[6]; // the 240 boundary — post-release
    let released = released_checkpoint
        .cohorts
        .iter()
        .find(|cohort| cohort.cohort == engine_slice::soak_scenario::HELD_COHORT)
        .expect("the held cohort is accounted");
    assert!(
        released.held == 0 && released.executed == released.expected,
        "the held cohort released through the tour's reactivation and executed: {released:?}"
    );
    // The overload's measurable bounded backlog: at the end of the
    // sustained-overload phase (the 190 boundary) the cohort is still
    // outstanding; at finalization it has drained.
    let overload_checkpoint = &scenario.checkpoints[3]; // the 190 boundary
    let overload = overload_checkpoint
        .cohorts
        .iter()
        .find(|cohort| cohort.cohort == engine_slice::soak_scenario::OVERLOAD_COHORT)
        .expect("the overload cohort is accounted");
    assert!(
        overload.queued + overload.blocked + overload.held > 0,
        "the sustained overload leaves a measurable backlog: {overload:?}"
    );
    for cohort in &scenario.terminal.cohorts {
        assert!(
            cohort.complete(),
            "the terminal accounting is complete: {cohort:?}"
        );
    }
    assert!(scenario.cohorts_drained());
    // The compressed fixture covers every phase, with its documented
    // differences (ONE action cycle in the short profile).
    assert!(scenario.coverage.all_phases_entered());
    assert!(scenario.coverage.horizon_reached);
    assert_eq!(
        scenario.actions.len(),
        1,
        "the short profile's single action cycle"
    );
    for action in &scenario.actions {
        assert!(
            action.accepted,
            "the scripted action fired: {}",
            action.detail
        );
    }
    // The run itself closed clean (the application-level surfaces).
    assert!(report.application_failure.is_none());
    assert!(report.close.failure.is_none());
    assert!(!report.scenario_failed());
}

/// §7D: a failed evidence package fails the scenario run VISIBLY (the
/// verdict is carried in the report — the CLI's exit rule includes
/// it), while the run's logical close is still reported
/// independently.
#[test]
fn a_failed_evidence_package_fails_the_run_visibly() {
    let cleaner = TestDir::new("evidence-failure");
    let mut options = scenario_options(&cleaner, SoakScenarioId::Stage6V1Short, None);
    options.evidence = Some(EvidenceConfig::new(
        cleaner.root.join("evidence"),
        "s6c3-failure",
    ));
    options.evidence_fail_writes = true;
    let report = run_persist_demo_headless(options).expect("the run completes");
    assert!(
        report.evidence_failed(),
        "the evidence verdict fails the run"
    );
    assert!(!report.scenario_failed(), "the logical gates still hold");
    assert!(
        report.close.failure.is_none(),
        "the close is independently clean"
    );
}

/// §7D + §10: the scenario run's evidence package — collected through
/// the accepted S6-C1 collector, independently verified, with the
/// single-site close observable in the persistence stream (exactly
/// one `close` sample) and the run's git provenance recorded.
#[test]
fn the_scenario_run_collects_and_verifies_an_evidence_package() {
    let cleaner = TestDir::new("evidence-package");
    let mut options = scenario_options(&cleaner, SoakScenarioId::Stage6V1Short, None);
    let evidence_dir = cleaner.root.join("evidence");
    options.evidence = Some(EvidenceConfig::new(evidence_dir.clone(), "stage6-v1-short"));
    let report = run_persist_demo_headless(options).expect("the run completes");
    let outcome = report.evidence.as_ref().expect("the evidence outcome");
    assert!(outcome.is_complete(), "the package is complete");
    let verification = verify_package(&evidence_dir).expect("the package verifies");
    assert!(verification.verified_complete());
    // The single-site close: exactly ONE close latency sample in the
    // persistence stream.
    let persistence = std::fs::read_to_string(evidence_dir.join("persistence.jsonl")).unwrap();
    let close_samples = persistence
        .lines()
        .filter(|line| {
            matches!(
                engine_slice::soak_evidence::decode_line(line),
                Ok(EvidenceRecord::Persistence(sample))
                    if matches!(sample.kind, PersistenceKind::Close)
            )
        })
        .count();
    assert_eq!(close_samples, 1, "the graceful close ran exactly once");
    // The ticks stream carries one sample per scripted tick.
    let ticks = std::fs::read_to_string(evidence_dir.join("ticks.jsonl")).unwrap();
    assert_eq!(ticks.lines().count(), 300);
    // The manifest carries the run's provenance.
    let manifest = std::fs::read_to_string(evidence_dir.join("manifest.json")).unwrap();
    assert!(
        manifest.contains("\"git_commit\""),
        "the git provenance is recorded"
    );
    assert!(
        manifest.contains("stage6-v1-short"),
        "the scenario label is recorded"
    );
}

/// §6: an unable-to-drain fixed-step debt is REPORTED, never
/// discarded — the bounded deadline expires with the remainder
/// outstanding, the absolute time accounting still holds, and the
/// campaign still closes through the single site.
#[test]
fn an_unable_to_drain_debt_is_reported_not_discarded() {
    let cleaner = TestDir::new("drain-timeout");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&cleaner.root, SimConfig::default()).expect("the open");
    // A deep stall accrues real debt: 30 seconds of wall time — 300
    // ticks due, the bounded catch-up executes 8, the remainder is
    // retained.
    demo.advance(Duration::from_secs(30))
        .expect("the stall advance");
    let outstanding = demo.outstanding_ticks();
    assert!(outstanding > 0, "the stall retained fixed-step debt");
    // A zero-second stabilization deadline: the drain may not run at
    // all — the failure is immediate, bounded, and honest.
    let drain = demo.drain_bounded(0);
    assert!(!drain.completed, "the drain reports the timeout");
    assert!(
        drain.refused.is_none(),
        "no tick refused — only the deadline"
    );
    assert_eq!(
        drain.outstanding_after, drain.outstanding_before,
        "the debt is retained, never silently discarded"
    );
    assert_eq!(drain.outstanding_after, outstanding);
    assert!(
        drain.time_accounting_holds,
        "the absolute time accounting still holds (the debt is a real obligation)"
    );
    assert_eq!(
        demo.ticks_due_total(),
        demo.session().clock().as_u64() + u64::from(demo.outstanding_ticks()),
        "ticks due == campaign clock + outstanding"
    );
    // The debt remains drainable through the accepted path, and the
    // campaign still closes through the single site.
    let drain = demo.drain_bounded(60);
    assert!(
        drain.completed,
        "the retained debt drains when the deadline allows"
    );
    assert_eq!(demo.outstanding_ticks(), 0);
    let finalization = demo.finalize(None);
    assert!(
        finalization.close_summary().failure.is_none(),
        "the close completes"
    );
}

/// §7C: the closed scenario campaign recovers in a FRESH PROCESS (the
/// real binary) — the exact durable state restored, the continuation
/// closes clean, and no input replay is needed.
#[test]
fn fresh_process_recovery_after_the_scenario_close() {
    let root = std::env::temp_dir().join(format!(
        "s7-s6c3-recovery-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let run = |extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_engine-slice"))
            .args(["--demo-persist", root.to_str().unwrap(), "--headless"])
            .args(extra)
            .output()
            .expect("the binary spawns");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    };
    let (code, first) = run(&["--scenario", "stage6-v1-short"]);
    assert_eq!(code, 0, "the scenario run exits clean:\n{first}");
    let closed_fingerprint = first
        .lines()
        .find(|line| line.contains("stage4 fingerprint"))
        .and_then(|line| line.split_whitespace().last())
        .expect("the closed fingerprint prints")
        .to_string();
    assert!(first.contains("closed at tick 300"));
    let (code, second) = run(&["--ticks", "30"]);
    assert_eq!(code, 0, "the recovery run exits clean:\n{second}");
    assert!(
        second.contains("campaign: recovered"),
        "the relaunch recovers"
    );
    assert!(second.contains("recovered at tick 300"));
    assert!(second.contains("closed at tick 330"));
    assert!(
        second.contains(&closed_fingerprint),
        "the restored fingerprint equals the closed fingerprint ({closed_fingerprint}):\n{second}"
    );
    assert!(second.contains("conservation true"));
    let _ = std::fs::remove_dir_all(&root);
}

/// §7D: an EXTERNALLY terminated run (the close status unknown) — the
/// killed process leaves whatever durable state exists, and the
/// relaunch recovers honestly from it.
#[test]
fn an_interrupted_run_recovers_from_the_durable_state() {
    let root = std::env::temp_dir().join(format!(
        "s7-s6c3-interrupted-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine-slice"))
        .args([
            "--demo-persist",
            root.to_str().unwrap(),
            "--headless",
            "--scenario",
            "stage6-v1",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the binary spawns");
    // The full 9000-tick profile runs long enough to terminate
    // mid-campaign (a bounded kill window — never an unbounded wait).
    std::thread::sleep(Duration::from_secs(2));
    child.kill().expect("the kill lands");
    let status = child.wait().expect("the wait reaps");
    assert!(!status.success(), "the killed run did not exit cleanly");
    let output = Command::new(env!("CARGO_BIN_EXE_engine-slice"))
        .args([
            "--demo-persist",
            root.to_str().unwrap(),
            "--headless",
            "--ticks",
            "30",
        ])
        .output()
        .expect("the relaunch spawns");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the interrupted campaign recovers (exit {:?}):\n{stdout}",
        output.status.code()
    );
    assert!(stdout.contains("campaign: recovered"));
    assert!(stdout.contains("conservation true"));
    let _ = std::fs::remove_dir_all(&root);
}

/// The S6-C2-deferred watermark check, at checkpoint precision: the
/// safe watermark never overclaims past a pending earlier-due event
/// and retreats when a late arrival lands behind it.
#[test]
fn the_watermark_retreats_on_late_arrivals() {
    let cleaner = TestDir::new("watermark");
    let location = StoreLocation::Database(cleaner.root.join("campaign.db"));
    let mut store = SessionStore::open(&location).unwrap();
    store
        .append(
            &RevisionRecord::founding(AuthorityMode::Standalone, "s6c3-watermark", 0),
            &[],
            None,
        )
        .unwrap();
    let (spine, director) = demo_scenario(
        &SimConfig::default(),
        &[engine_slice::persist_demo::DEMO_PRESENCE],
    )
    .expect("the scenario builds");
    let journal = T1Journal::open(cleaner.root.join("journal.t1")).unwrap();
    let mut session =
        OrchestratedSession::open(spine, director, &location, journal, "s6c3-watermark")
            .expect("the session opens");
    let presence = &[engine_slice::persist_demo::DEMO_PRESENCE];
    for _ in 0..40 {
        session.tick(presence).expect("the tick runs");
    }
    let before = session.spine().processed_through();
    // A late arrival: due BEHIND the current clock (and behind the
    // watermark when one exists) — scheduled through the accepted
    // boundary, reported as a late arrival, never shifted.
    let spec = EventSpec::new(
        EventClass::new(0),
        Schedule::AtLogicalTime { due: Tick(2) },
        engine_slice::soak_scenario::burst_payload(9, 0),
    )
    .in_field(Field::Near);
    let id = session
        .schedule_event(spec)
        .expect("the late arrival is accepted");
    assert!(
        session.spine().stats().late_arrivals > 0,
        "the late arrival is reported"
    );
    session.tick(presence).expect("the tick runs");
    let after = session.spine().processed_through();
    let min_pending = session.spine().min_pending_due();
    let max_executed = session.spine().max_executed_due();
    if let (Some(watermark), Some(pending)) = (after, min_pending) {
        assert!(
            watermark <= pending,
            "the watermark never overclaims past a pending earlier-due event"
        );
    }
    if let (Some(watermark), Some(executed)) = (after, max_executed) {
        assert!(
            watermark <= executed,
            "the watermark stays <= max executed due"
        );
    }
    if let (Some(before), Some(after)) = (before, after) {
        assert!(
            after <= Tick(2) || after <= before,
            "the watermark retreats (or bounds to the late arrival's due) behind the late event \
             (before {before}, after {after})"
        );
    }
    assert!(session.spine().is_executed(id) || session.spine().pending_count() > 0);
    let close = session.close();
    assert!(close.failure.is_none(), "the close completes");
}

/// C3-F1: the checkpoint regression — checkpoint N is the state AFTER
/// precisely N intended logical ticks (the actual campaign clock AND
/// scenario tick both equal the boundary), never the pre-tick state
/// and never N−1. The initial, burst, dormancy, and final boundaries
/// are covered, with the off-by-one discriminators: a cohort whose
/// first injection is the INPUT for tick N+1 must be ABSENT from
/// checkpoint N and fully present at the later boundary.
#[test]
fn checkpoints_observe_the_state_after_precisely_n_logical_ticks() {
    let cleaner = TestDir::new("checkpoint-boundary");
    let report = run_persist_demo_headless(scenario_options(
        &cleaner,
        SoakScenarioId::Stage6V1Short,
        None,
    ))
    .expect("the scenario run completes");
    let scenario = report.scenario.as_ref().expect("the scenario report");
    // The recorded boundaries, in order: [60, 110, 150, 190, 230,
    // 234, 240, 266] — each must record the AFTER-tick state.
    let boundaries = SoakScenarioId::Stage6V1Short.checkpoints();
    assert_eq!(scenario.checkpoints.len(), boundaries.len());
    for (checkpoint, boundary) in scenario.checkpoints.iter().zip(boundaries) {
        assert_eq!(
            checkpoint.campaign_tick, *boundary,
            "the actual campaign clock at the checkpoint is the completed boundary tick"
        );
        assert_eq!(
            checkpoint.scenario_tick, *boundary,
            "the scripted-tick count at the checkpoint is the completed boundary tick"
        );
        assert!(checkpoint.conservation);
    }
    let has_cohort = |index: usize, cohort: u16| {
        scenario.checkpoints[index]
            .cohorts
            .iter()
            .find(|entry| entry.cohort == cohort)
    };
    // The initial boundary (60): the cluster's injection is the INPUT
    // for tick 61 — absent at 60, fully present at 110 (with the
    // stream cohort).
    assert!(
        has_cohort(0, engine_slice::soak_scenario::CLUSTER_COHORT).is_none(),
        "the cluster (input 60) is NOT yet applied at the tick-60 checkpoint"
    );
    let at_110 = has_cohort(1, engine_slice::soak_scenario::CLUSTER_COHORT)
        .expect("the cluster is accounted at the tick-110 checkpoint");
    assert_eq!(at_110.expected, 200);
    assert_eq!(
        has_cohort(1, engine_slice::soak_scenario::STREAM_COHORT)
            .expect("the stream cohort at 110")
            .expected,
        60
    );
    // The burst boundary (150): the overload's first injection is the
    // INPUT for tick 151 — absent at 150, fully expected at 190.
    assert!(
        has_cohort(2, engine_slice::soak_scenario::OVERLOAD_COHORT).is_none(),
        "the overload (first input 150) is NOT yet applied at the tick-150 checkpoint"
    );
    let at_190 = has_cohort(3, engine_slice::soak_scenario::OVERLOAD_COHORT)
        .expect("the overload is accounted at the tick-190 checkpoint");
    assert_eq!(at_190.expected, 9600);
    // The dormancy boundaries (234 mid-hold, 240 released) and the
    // final boundary (266) are asserted by the checkpoint-series test.
}

/// C3-F2A: the independent evidence verification fails closed on
/// EVERY outcome — only `Ok(verified complete)` passes; an incomplete
/// verification and a TYPED VERIFIER ERROR both fail the run with the
/// actual failure preserved (an error is never swallowed into a
/// None).
#[test]
fn evidence_verification_fails_closed_on_every_outcome() {
    let complete = PackageVerification {
        schema_version: "s6-evidence/2".into(),
        scenario: "stage6-v1-short".into(),
        declared_complete: true,
        streams: Vec::<VerifiedStream>::new(),
        problems: Vec::<String>::new(),
    };
    assert!(soak_evidence_verification_failure(&Ok(complete)).is_none());
    let incomplete = PackageVerification {
        schema_version: "s6-evidence/2".into(),
        scenario: "stage6-v1-short".into(),
        declared_complete: false,
        streams: Vec::<VerifiedStream>::new(),
        problems: vec!["the ticks stream lost 3 records".into()],
    };
    let failure = soak_evidence_verification_failure(&Ok(incomplete))
        .expect("an incomplete verification fails the run");
    assert!(failure.contains("did not verify complete"));
    assert!(
        failure.contains("ticks stream lost 3 records"),
        "the actual problem is preserved: {failure}"
    );
    let error = soak_evidence_verification_failure(&Err(EvidenceError::Decode(
        "the manifest is not parseable".into(),
    )))
    .expect("a typed verifier error fails the run");
    assert!(
        error.contains("independent evidence verification failed"),
        "the typed error is preserved: {error}"
    );
    assert!(error.contains("manifest is not parseable"));
}

/// C3-F2A: a graphical soak without its required evidence
/// configuration fails visibly BEFORE anything opens (the library's
/// own fail-closed check, ahead of the renderer and the campaign).
#[test]
fn a_graphical_soak_without_its_required_configuration_fails_before_the_window() {
    let cleaner = TestDir::new("soak-missing-evidence");
    let mut options = scenario_options(&cleaner, SoakScenarioId::Stage6V1Short, None);
    options.headless = false;
    options.soak_wall_seconds = Some(30);
    options.evidence = None;
    let error = run_persist_demo(options).expect_err("the missing configuration fails");
    assert!(
        error.contains("requires --scenario and --evidence-dir"),
        "the visible error names the requirement: {error}"
    );
}

/// C3-F2C: the soak summary's publication is atomic and fails closed
/// — a failed write never truncates an existing accepted summary (the
/// tmp-then-rename protocol), and an error is returned, never
/// swallowed.
#[test]
fn the_soak_summary_publication_is_atomic_and_fails_closed() {
    let cleaner = TestDir::new("summary-publication");
    let campaign = cleaner.root.join("campaign");
    std::fs::create_dir_all(&campaign).unwrap();
    // The healthy publication: the final name appears, no staging
    // file remains.
    let path = publish_soak_summary(&campaign, "{\"schema\":\"s6-soak-summary/1\"}")
        .expect("the healthy publication succeeds");
    assert_eq!(path, campaign.join("soak-summary.json"));
    assert!(path.exists());
    assert!(!campaign.join("soak-summary.json.tmp").exists());
    // The accepted summary is never truncated by a failed write: the
    // staging path is blocked, the publication fails, and the prior
    // summary is byte-identical.
    let prior = std::fs::read_to_string(&path).unwrap();
    std::fs::create_dir_all(campaign.join("soak-summary.json.tmp")).unwrap();
    let error = publish_soak_summary(&campaign, "{\"schema\":\"broken\"")
        .expect_err("the blocked staging path fails the publication");
    assert!(!error.to_string().is_empty());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        prior,
        "the prior accepted summary is untouched"
    );
}

/// C3-F2B: the drain gate — a completed drain MUST also hold the
/// absolute time accounting; a false conservation fails the workload
/// gate exactly like an incomplete drain or a refusal.
#[test]
fn a_completed_drain_with_false_time_conservation_fails_the_gate() {
    let healthy = SoakDrain {
        deadline_secs: 300,
        drain_ms: 1.0,
        outstanding_before: 12,
        outstanding_after: 0,
        executed_ticks: 12,
        completed: true,
        time_accounting_holds: true,
        refused: None,
    };
    assert!(soak_drain_gate(&healthy).is_none());
    let false_conservation = SoakDrain {
        time_accounting_holds: false,
        ..healthy.clone()
    };
    let failure = soak_drain_gate(&false_conservation)
        .expect("a completed drain with false time accounting fails");
    assert!(failure.contains("time accounting does not hold"));
    let incomplete = SoakDrain {
        completed: false,
        outstanding_after: 5,
        ..healthy
    };
    assert!(soak_drain_gate(&incomplete).is_some_and(|failure| failure.contains("did not drain")));
}

/// C3-F3: the scenario verdict requires the FULL versioned workload —
/// each failure named. The arms are exercised against a real healthy
/// report (one mutated field per arm): a missing required cohort, an
/// unexpected rejection, a refused action, a pending (never
/// completed) action, a compacted-history downgrade, an outstanding
/// drainable cohort, and a publication workload without durable
/// completions.
#[test]
fn the_scenario_verdict_requires_the_full_workload() {
    let cleaner = TestDir::new("verdict-arms");
    let report = run_persist_demo_headless(scenario_options(
        &cleaner,
        SoakScenarioId::Stage6V1Short,
        None,
    ))
    .expect("the scenario run completes");
    let healthy = report.scenario.as_ref().expect("the scenario report");
    assert!(healthy.verdict_ok(), "the healthy run passes every gate");
    let mut missing_cohort = healthy.clone();
    missing_cohort
        .terminal
        .cohorts
        .retain(|cohort| cohort.cohort != engine_slice::soak_scenario::OVERLOAD_COHORT);
    assert!(
        missing_cohort
            .verdict_failures()
            .iter()
            .any(|failure| failure.contains("required cohort 5 is missing"))
    );
    let mut rejected = healthy.clone();
    rejected.rejected = 1;
    assert!(
        rejected
            .verdict_failures()
            .iter()
            .any(|failure| failure.contains("planned injection(s) were rejected"))
    );
    let mut refused_action = healthy.clone();
    refused_action.actions[0].accepted = false;
    assert!(
        refused_action
            .verdict_failures()
            .iter()
            .any(|failure| failure.contains("was REFUSED"))
    );
    let mut pending_action = healthy.clone();
    pending_action.actions[0].completed = None;
    pending_action.actions[0].outcome = None;
    assert!(
        pending_action
            .verdict_failures()
            .iter()
            .any(
                |failure| failure.contains("did not COMPLETE") && failure.contains("still pending")
            )
    );
    let mut compacted = healthy.clone();
    compacted.terminal.identity_evidence = "by-subtraction";
    compacted.terminal.history_full = false;
    assert!(
        compacted
            .verdict_failures()
            .iter()
            .any(|failure| failure.contains("NOT VERIFIED") && failure.contains("by-subtraction"))
    );
    let mut outstanding = healthy.clone();
    for cohort in &mut outstanding.terminal.cohorts {
        if cohort.cohort == engine_slice::soak_scenario::DRAIN_COHORT {
            cohort.queued = 7;
            cohort.executed -= 7;
        }
    }
    assert!(
        outstanding
            .verdict_failures()
            .iter()
            .any(|failure| failure.contains("remain outstanding at the terminal boundary"))
    );
    let mut unpublished = healthy.clone();
    unpublished.publication_completions = Some(1);
    assert!(
        unpublished
            .verdict_failures()
            .iter()
            .any(|failure| failure.contains("did not complete durably"))
    );
}

/// C3-F3 (real induction): an INTERRUPTED scenario — the replay capped
/// mid-lifecycle — fails the workload verdict with the honest named
/// failures: the pending (never-completed) action, the cohorts that
/// were never injected, and the drainable cohort still outstanding.
#[test]
fn an_interrupted_scenario_fails_the_workload_verdict() {
    let cleaner = TestDir::new("interrupted-verdict");
    let mut options = scenario_options(&cleaner, SoakScenarioId::Stage6V1Short, None);
    options.headless_ticks = 118; // the action fired at 115 — mid-lifecycle
    let report = run_persist_demo_headless(options).expect("the run completes");
    let scenario = report.scenario.as_ref().expect("the scenario report");
    let failures = scenario.verdict_failures();
    assert!(
        !failures.is_empty(),
        "the interrupted scenario fails the verdict"
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure
                .contains("did not COMPLETE through the applied-credits lifecycle")),
        "the pending action is named: {failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("required cohort 5 is missing")),
        "the never-injected overload cohort is named: {failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("remain outstanding")),
        "the drainable cohort still outstanding is named: {failures:?}"
    );
    assert!(report.scenario_failed());
    // The observational accounting is preserved — the verdict fails on
    // the workload, never by hiding the state.
    assert!(scenario.terminal.conservation);
}

/// C3-F3B (real induction): a COMPACTED execution history downgrades
/// the identity-level verdict to NOT VERIFIED — the observational
/// by-subtraction accounting is preserved and labeled, and never
/// silently upgraded to an exact-once PASS.
#[test]
fn a_compacted_history_downgrades_the_identity_verdict() {
    let cleaner = TestDir::new("compacted-downgrade");
    let location = StoreLocation::Database(cleaner.root.join("campaign.db"));
    let mut store = SessionStore::open(&location).unwrap();
    store
        .append(
            &RevisionRecord::founding(AuthorityMode::Standalone, "s6c3-compacted", 0),
            &[],
            None,
        )
        .unwrap();
    let (spine, director) = demo_scenario(
        &SimConfig::default(),
        &[engine_slice::persist_demo::DEMO_PRESENCE],
    )
    .expect("the scenario builds");
    let journal = T1Journal::open(cleaner.root.join("journal.t1")).unwrap();
    let mut session =
        OrchestratedSession::open(spine, director, &location, journal, "s6c3-compacted")
            .expect("the session opens");
    let mut scenario = SoakScenario::new(
        SoakScenarioId::Stage6V1Short,
        STAGE6_SCENARIO_SEED,
        session.director().config().centers,
    );
    // The manual harness drives the accepted interface sequence (the
    // same input ordering `run_ticks` applies) — bursts and ticks
    // only; the action/publication lifecycles are PersistDemo-level
    // and are not this regression's subject.
    for tick in 0..SoakScenarioId::Stage6V1Short.horizon() {
        let step = scenario.step();
        for burst in step.bursts {
            let spec = EventSpec::new(
                EventClass::new(burst.region),
                Schedule::AtLogicalTime {
                    due: Tick(tick + 1 + burst.due_offset),
                },
                engine_slice::soak_scenario::burst_payload(burst.cohort, burst.index),
            )
            .in_field(burst.field);
            let result = session.schedule_event(spec);
            scenario.record_scheduled(burst.cohort, burst.index, result);
        }
        session.tick(&[step.presence]).expect("the tick runs");
        // A genuine publication (the accepted interfaces), so the
        // maintenance checkpoint's reclaimable range is nonempty —
        // an explicit checkpoint that indexes nothing refuses by
        // design.
        if tick == 200 {
            let capture = session.capture_snapshot().expect("the capture");
            let handle = session.submit_publication(capture).expect("the submission");
            session
                .wait_publication(handle)
                .expect("the publication completes");
        }
        // Compact mid-run through the accepted maintenance interface:
        // the retained tail replaces the full history.
        if tick == 270 {
            session
                .publish_checkpoint(64)
                .expect("the checkpoint maintenance runs");
        }
    }
    let terminal = scenario.terminal_accounting(&session);
    assert_eq!(
        terminal.identity_evidence, "by-subtraction",
        "the compacted history is labeled honestly"
    );
    assert!(!terminal.history_full);
    for cohort in &terminal.cohorts {
        assert!(
            cohort.by_subtraction,
            "the accounting is preserved: {cohort:?}"
        );
    }
    let publications = engine_slice::soak_scenario::PublicationTerminal {
        pending: session.pending_publications(),
        tracked_pending: false,
    };
    let report = scenario.run_report(terminal, publications);
    assert!(
        report
            .verdict_failures()
            .iter()
            .any(|failure| failure.contains("NOT VERIFIED") && failure.contains("by-subtraction")),
        "the identity gate is downgraded, never silently upgraded"
    );
    let close = session.close();
    assert!(close.failure.is_none());
}

/// C3-F4 + C3-F5A: the cutoff feasibility and attribution regression.
/// The window's accrual stops STRICTLY before the last frame's
/// interval (the cutoff fires at the frame top, before
/// `begin_frame`), so at an exact-horizon window the horizon's final
/// scripted tick is structurally NEVER consumed — the in-window
/// scripted count is horizon − 1 (or less), no accrued debt remains
/// for the drain, and the workload-completeness gate still passes
/// with its documented headroom while the horizon is honestly
/// reported unreached.
#[test]
fn the_cutoff_attributed_feasibility_holds_at_an_exact_horizon_window() {
    // The structural arithmetic, for the frame-rate matrix (the full
    // profile's 900 s window at 10 Hz): the in-window tick count is
    // floor((window − frame_interval) / period) — strictly below the
    // horizon for EVERY frame interval, and far above the
    // workload-completeness boundary.
    let window_ms = 900_000.0_f64;
    for frame_ms in [6.944_f64, 8.333, 16.667, 33.333, 100.0] {
        let in_window = ((window_ms - frame_ms) / 100.0).floor() as u64;
        assert!(
            in_window < 9000,
            "the horizon's final tick is structurally post-cutoff at {frame_ms}ms frames"
        );
        assert!(
            in_window >= 7800,
            "the workload-completeness boundary is covered with headroom at {frame_ms}ms frames"
        );
    }
    // The behavioral regression over the real driver: the compressed
    // profile's exact-horizon window (300 ticks = 30.0 s), emulating
    // the graphical loop's cutoff rule (the frame that would cross
    // the boundary never accrues).
    let cleaner = TestDir::new("cutoff-feasibility");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&cleaner.root, SimConfig::default()).expect("the open");
    demo.attach_scenario(SoakScenario::new(
        SoakScenarioId::Stage6V1Short,
        STAGE6_SCENARIO_SEED,
        demo.session().director().config().centers,
    ));
    let frame = Duration::from_millis(16);
    let window = Duration::from_millis(30_000);
    let mut accrued = Duration::ZERO;
    while accrued + frame < window {
        demo.advance(frame).expect("the frame advance runs");
        accrued += frame;
    }
    let scripted_at_cutoff = demo.scenario().expect("the scenario ran").scenario_tick();
    assert!(
        scripted_at_cutoff < 300,
        "the horizon is structurally post-cutoff (scripted {scripted_at_cutoff} in window)"
    );
    assert!(
        scripted_at_cutoff >= 266,
        "the workload-completeness boundary is covered in window"
    );
    let coverage = demo.scenario().expect("the scenario ran").coverage();
    assert!(coverage.all_phases_entered());
    assert!(
        !coverage.horizon_reached,
        "the horizon is honestly NOT reached at the cutoff"
    );
    assert!(
        coverage_gate_failure(&coverage, scripted_at_cutoff, 266).is_none(),
        "the corrected coverage gate passes the healthy exact-horizon window"
    );
    assert!(
        coverage_gate_failure(&coverage, 200, 266).is_some(),
        "a window short of the workload boundary fails the gate"
    );
    // The drain settles the ACCRUED debt — and at an exact-horizon
    // window there is none: the accrual stopped strictly (sub-tick)
    // short of the horizon's final tick, and no new wall time accrues
    // post-cutoff, so the horizon's final scripted input is honestly
    // NEVER consumed — in-window, in-drain, or otherwise. The
    // workload-completeness gate (266) passes; the horizon is
    // reported unreached; the campaign closes one tick short with
    // every workload obligation long since complete.
    let drain = demo.drain_bounded(60);
    assert!(
        drain.completed,
        "the drain settles (there is no accrued debt)"
    );
    assert_eq!(
        drain.executed_ticks, 0,
        "the accrual is sub-tick short of the horizon — no debt remains"
    );
    assert_eq!(
        demo.scenario().expect("the scenario ran").scenario_tick(),
        scripted_at_cutoff,
        "the terminal scripted count equals the in-window count (the horizon's final \
         input is never consumed)"
    );
    let finalization = demo.finalize(None);
    assert!(finalization.close_summary().failure.is_none());
}

/// C3-F5B: the namespaced integrated-soak flags reject every
/// top-level mode outside `--demo-persist` (a visible nonzero error),
/// while the Stage 1 `--soak` legacy command is preserved.
#[test]
fn top_level_soak_flags_reject_other_modes() {
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_engine-slice"))
            .args(args)
            .output()
            .expect("the binary spawns");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    for args in [
        vec!["--soak-wall-seconds", "900"],
        vec!["--scenario", "stage6-v1"],
        vec!["--scenario-seed", "7"],
        vec!["--evidence-dir", "/tmp/s7-unused"],
        vec!["--stabilization-deadline-secs", "30"],
        vec!["--sim", "--scenario", "stage6-v1"],
        vec!["--render", "--soak-wall-seconds", "30"],
    ] {
        let (code, stderr) = run(&args);
        assert_eq!(code, 1, "the top-level flags reject ({args:?}):\n{stderr}");
        assert!(
            stderr.contains("require --demo-persist"),
            "the visible error names the requirement: {stderr}"
        );
    }
    // The Stage 1 legacy command is unchanged: the paced SDL shell
    // soak still runs (the dummy driver — no Vulkan, no simulation,
    // no persistence).
    let (code, _stderr) = run(&["--soak", "1"]);
    assert_eq!(code, 0, "the Stage 1 --soak legacy command is preserved");
}

/// The hardware-readiness corrective: the formal validation-enabled
/// soak's configuration gate — a soak that REQUESTED the validation
/// layer must also ACHIEVE it. Requested-but-unavailable fails the
/// gate; an unrequested (validation-disabled) soak never fails on
/// availability; the renderer's transparent fallback is unchanged
/// for every other mode.
#[test]
fn the_validation_availability_gate_fails_a_requested_but_inactive_layer() {
    assert!(
        soak_validation_gate(true, false).is_some_and(|failure| {
            failure.contains("REQUESTED") && failure.contains("configuration gate")
        }),
        "requested-but-unavailable fails the formal soak's configuration gate"
    );
    assert!(
        soak_validation_gate(true, true).is_none(),
        "requested and achieved passes"
    );
    assert!(
        soak_validation_gate(false, false).is_none(),
        "a validation-DISABLED soak never fails on availability"
    );
    assert!(
        soak_validation_gate(false, true).is_none(),
        "an unrequested-but-available layer is fine (auto behavior unchanged)"
    );
}

/// The hardware-readiness corrective: the published soak summary
/// IDENTIFIES the verified evidence package — the ACTUAL configured
/// directory (never an inferred path), the independently verified
/// package's schema version, the verified-complete status, the
/// validation requested/achieved state with its counts, and the
/// missing-to-horizon progress at the cutoff.
#[test]
fn the_published_summary_identifies_the_verified_evidence_package() {
    let cleaner = TestDir::new("summary-provenance");
    let report = run_persist_demo_headless(scenario_options(
        &cleaner,
        SoakScenarioId::Stage6V1Short,
        None,
    ))
    .expect("the scenario run completes");
    let scenario = report.scenario.as_ref().expect("the scenario report");
    let measurement = engine_slice::persist_demo::SoakMeasurement {
        wall_seconds: 900,
        start: std::time::Instant::now(),
        cutoff_reached: true,
        cutoff: Some(engine_slice::persist_demo::SoakCutoff {
            wall_duration_ms: 900_012.5,
            campaign_tick: 8999,
            outstanding_debt: 0,
            scenario_tick_at_cutoff: 299,
            coverage: Some(engine_slice::soak_scenario::CoverageReport {
                phases: Vec::new(),
                horizon_reached: false,
            }),
        }),
        drain: None,
    };
    let evidence_dir = cleaner.root.join("evidence");
    let context = engine_slice::persist_demo::SoakSummaryContext {
        evidence_dir: Some(evidence_dir.clone()),
        evidence_schema: Some("s6-evidence/2".into()),
        evidence_verified: true,
        validation_requested: true,
        validation_active: true,
        validation_errors: 0,
        validation_warnings: 0,
    };
    let summary = engine_slice::persist_demo::soak_summary_json(
        &measurement,
        Some(scenario),
        None,
        Some(true),
        &context,
        None,
        &SimConfig::default(),
        "0123abcd",
        false,
    );
    // The serializer JSON-escapes the path's backslashes (Windows:
    // "C:\\Users\\..." in the document) — the expected substring must
    // be the JSON-escaped form of the directory, not the raw path.
    let expected_dir = evidence_dir.display().to_string().replace('\\', "\\\\");
    assert!(
        summary.contains(&format!(
            "\"evidence\":{{\"dir\":\"{}\",\"schema\":\"s6-evidence/2\",\"verified\":true}}",
            expected_dir
        )),
        "the summary names the ACTUAL configured evidence directory, the schema \
         version, and the verified status: {summary}"
    );
    assert!(
        summary.contains(
            "\"validation\":{\"requested\":true,\"active\":true,\"errors\":0,\"warnings\":0}"
        ),
        "the validation state and counts are recorded: {summary}"
    );
    assert!(
        summary.contains("\"missing_to_horizon\":1"),
        "the missing-to-horizon progress at the cutoff is reported: {summary}"
    );
    assert!(summary.contains("\"workload_complete_tick\":266"));
    assert!(summary.contains("\"cutoff_scripted_ticks_in_window\":299"));
    assert!(summary.contains("\"cutoff_campaign_tick\":8999"));
}
