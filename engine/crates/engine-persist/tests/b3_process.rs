//! Real-process termination at the FULL Batch 3 stack — a child
//! process drives the durable T1 journal + the `LocalFileMedium` +
//! the `CommitPipeline` to a precise crash point and dies by hard
//! exit (no destructors, no publication); the parent — an independent
//! process — reconstructs from disk alone and proves the crash-window
//! honesty end to end: the four-state acknowledgment discipline (a
//! durable transaction is committed and actionable, never a
//! fabricated acknowledgment across the boundary), exactly-once
//! recovery under the original identities, and canonical confirmation
//! only through the verified chain walk.
//!
//! `tests/journal_process.rs` covers the journal alone; this suite
//! composes the pipeline: the W2 window (death after T1, before live
//! application) and the W4 window (death after live application,
//! before any publication — nothing durable claims representation, so
//! recovery re-derives and the re-drive confirms exactly once).

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use engine_persist::{
    AppendOutcome, AuthorityMode, AwardCheckpoint, CanonicalMedium, ClassifiedSpan, CommitOutcome,
    CommitPipeline, ConsequenceEnvelope, ConsequenceId, CounterValues, Coverage, DurabilityClass,
    FrontierEntry, Hash, KeyPhase, LocalFileMedium, OccurrenceSpan, RecoveryAnchor, RevisionRecord,
    SignalLane, SourceBaseline, SourcePosition, T1Journal, TransactionDurability, TransactionId,
    reconcile,
};

const ROLE_ENV: &str = "S7_B3_ROLE";
const PATH_ENV: &str = "S7_B3_PATH";
const PHASE_ENV: &str = "S7_B3_PHASE";
const GO_ENV: &str = "S7_B3_GO";
const RESULT_ENV: &str = "S7_B3_RESULT";

const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(10);

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

/// A unique temp root removed on drop — no `tempfile` dependency; the
/// crate's dependency set is fixed by the authorization.
struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let seq = DIR_SEQ.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("s7-b3-proc-{}-{label}-{seq}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test root");
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn store(&self) -> PathBuf {
        self.0.join("store")
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.t1")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ------------------------------------------------------------- fixtures

fn envelope(id: ConsequenceId, span: (u64, u64)) -> ConsequenceEnvelope {
    ConsequenceEnvelope::capture(
        id,
        vec![ClassifiedSpan {
            span: OccurrenceSpan::new(span.0, span.1).unwrap(),
            class: DurabilityClass::DurableEligible,
        }],
        SignalLane::Captured,
        CounterValues::default(),
        SourceBaseline {
            counters: CounterValues::default(),
            award: AwardCheckpoint {
                durable: Coverage::new(),
                provisional: Coverage::new(),
            },
        },
        SourcePosition {
            head: None,
            tick: 110,
        },
    )
    .unwrap()
}

fn first_id() -> ConsequenceId {
    ConsequenceId::new(1, 1, 1)
}

fn second_id() -> ConsequenceId {
    ConsequenceId::new(1, 2, 1)
}

fn counters(milestones: u64, lane_value: u64) -> CounterValues {
    CounterValues {
        milestones,
        pending_signals: 0,
        captured: lane_value,
        credits: 0,
    }
}

fn assert_recovery_invariants(outcome: &engine_persist::RecoveryOutcome) {
    for (id, participant) in &outcome.recovered {
        assert!(
            participant.counters.conservation_holds(),
            "conservation must hold for {id} after every recovery: {:?}",
            participant.counters
        );
        assert!(
            participant.award.validate().is_ok(),
            "the recovered award lanes stay cross-lane disjoint for {id}"
        );
    }
}

// ------------------------------------------------------------- child side

/// The child-process entry: dispatches on the harness environment. In
/// a normal `cargo test` run the role variable is absent and this is
/// a no-op — the parent spawns this same binary with `--exact
/// child_entry` and the variables set.
#[test]
fn child_entry() {
    let Some(role) = env::var(ROLE_ENV).ok() else {
        return;
    };
    match role.as_str() {
        "pcrasher" => child_crasher(),
        other => panic!("unknown role: {other}"),
    }
}

/// Drives the full stack to the crash point and dies by hard exit:
/// founds the store, journals ONE multi-identity boundary transaction
/// (two staged envelopes, one capture — the multi-key revision), and
/// — for the `w4` phase — applies the effects to live state before
/// dying. Nothing is ever published; no destructors run; the OS
/// reclaims the journal flock and the store locks — exactly the
/// process death the crash windows describe.
fn child_crasher() {
    wait_for_go();
    let root = PathBuf::from(env::var(PATH_ENV).expect("path env"));
    let phase = env::var(PHASE_ENV).unwrap_or_default();
    {
        let mut medium = LocalFileMedium::open(root.join("store")).unwrap();
        if medium.head().unwrap().is_none() {
            let founding =
                RevisionRecord::founding(AuthorityMode::Standalone, "b3-process-campaign", 0);
            match medium.append(&founding, &[], None).unwrap() {
                AppendOutcome::Accepted { .. } => {}
                other => panic!("the founding must accept: {other:?}"),
            }
        }
    }
    let journal = T1Journal::open(root.join("journal.t1")).unwrap();
    let medium = LocalFileMedium::open(root.join("store")).unwrap();
    let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
    pipeline.stage(envelope(first_id(), (0, 2))).unwrap();
    pipeline.stage(envelope(second_id(), (2, 4))).unwrap();
    let receipt = pipeline
        .capture(
            SourcePosition {
                head: None,
                tick: 110,
            },
            DurabilityClass::DurableEligible,
            Vec::new(),
            vec![FrontierEntry {
                region: 1,
                episode: 2,
            }],
            b"b3-process-plan".to_vec(),
        )
        .unwrap();
    let mut lines = vec![format!("captured:{}", receipt.journal.id.0.hex())];
    if phase == "w4" {
        pipeline.mark_applied(&[first_id(), second_id()]).unwrap();
        lines.push("applied".to_string());
    }
    write_result(&lines.join("\n"));
    // Hard death: exit code 6, no drops, no publication.
    std::process::exit(6);
}

fn wait_for_go() {
    let go = env::var(GO_ENV).expect("go env");
    let deadline = Instant::now() + WAIT;
    while !Path::new(&go).exists() {
        assert!(
            Instant::now() < deadline,
            "the go barrier deadline was exceeded"
        );
        std::thread::sleep(POLL);
    }
}

fn write_result(text: &str) {
    let path = env::var(RESULT_ENV).expect("result env");
    fs::write(&path, text).expect("write result");
}

// ------------------------------------------------------------ parent side

fn spawn_child(phase: &str, harness: &Path) -> Child {
    let go = harness.join("go");
    let result = harness.join("result-0");
    let mut command = Command::new(env::current_exe().expect("current exe"));
    command
        .arg("--exact")
        .arg("child_entry")
        .env(ROLE_ENV, "pcrasher")
        .env(PHASE_ENV, phase)
        .env(PATH_ENV, harness.display().to_string())
        .env(GO_ENV, go.display().to_string())
        .env(RESULT_ENV, result.display().to_string());
    command.stdout(Stdio::null()).stderr(Stdio::null());
    command.spawn().expect("spawn child")
}

fn release_barrier(harness: &Path) {
    fs::write(harness.join("go"), b"go").expect("release the go barrier");
}

fn read_result(harness: &Path) -> String {
    fs::read_to_string(harness.join("result-0")).expect("read result file")
}

fn wait_bounded(child: &mut Child, label: &str) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().expect("try wait") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{label} exceeded the {WAIT:?} bounded wait — no hang is accepted");
        }
        std::thread::sleep(POLL);
    }
}

/// The parent's reconstruction after the child's death, shared by the
/// W2 and W4 windows: the four-state honesty, the exactly-once
/// recovery, and the canonical confirmation through the verified
/// chain walk — with the identity and accounting asserts at every
/// step.
fn reconstruct_confirm_and_verify(dir: &TestDir, expect_applied_marker: bool) {
    let result = read_result(dir.path());
    let lines: Vec<&str> = result.lines().collect();
    let hex = lines[0].strip_prefix("captured:").expect("captured prefix");
    let transaction = TransactionId(Hash::from_hex(hex).expect("transaction hex"));
    if expect_applied_marker {
        assert_eq!(lines.get(1), Some(&"applied"));
    } else {
        assert_eq!(lines.len(), 1, "the W2 child never applied: {result}");
    }
    let first = first_id();
    let second = second_id();

    // The four-state honesty across real death: the boundary
    // transaction is committed and actionable; the acknowledgment is
    // NEVER fabricated across the process boundary.
    {
        let journal = T1Journal::open(dir.journal()).unwrap();
        assert_eq!(journal.transactions().len(), 1);
        assert_eq!(
            journal.transaction_status(transaction),
            Some(TransactionDurability::Committed)
        );
        assert!(!journal.acknowledged_this_session(transaction));
        assert!(journal.is_actionable(transaction));
    }
    // The fresh pipeline seeds from the journal (durable across
    // sessions) and recovers exactly once from the pre-event anchor.
    // scan_file is the lock-free read — safe alongside the open
    // journal instance.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let anchor = RecoveryAnchor::new(SourcePosition {
        head: None,
        tick: 100,
    });
    let outcome = reconcile(&anchor, &scan, &BTreeSet::new()).unwrap();
    assert_recovery_invariants(&outcome);
    assert_eq!(
        outcome.recovered.get(&first).unwrap().counters,
        counters(2, 2),
        "the first identity recovers exactly once"
    );
    assert_eq!(
        outcome.recovered.get(&second).unwrap().counters,
        counters(2, 2),
        "the second identity recovers exactly once"
    );
    let mut pipeline = {
        let journal = T1Journal::open(dir.journal()).unwrap();
        let medium = LocalFileMedium::open(dir.store()).unwrap();
        CommitPipeline::open(medium, journal).unwrap()
    };
    assert_eq!(
        pipeline.status(first).unwrap().phase,
        KeyPhase::JournaledT1,
        "the fresh pipeline reports the durable T1 honestly"
    );
    assert_eq!(
        pipeline.status(second).unwrap().phase,
        KeyPhase::JournaledT1
    );
    // The re-drive under the original identities confirms
    // exactly-once — the multi-key revision, both keys individually.
    let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
        panic!("expected confirmation");
    };
    assert_eq!(confirmation.keys, vec![first, second]);
    assert_eq!(
        pipeline.status(first).unwrap().phase,
        KeyPhase::InConfirmedChain
    );
    assert_eq!(
        pipeline.status(second).unwrap().phase,
        KeyPhase::InConfirmedChain
    );
    assert_eq!(pipeline.confirmed_coverage(first).unwrap().len(), 1);
    assert_eq!(pipeline.confirmed_coverage(second).unwrap().len(), 1);
    // Post-confirmation: the chain tier suppresses any re-recovery.
    let confirmed: BTreeSet<ConsequenceId> = pipeline.confirmed_keys().into_iter().collect();
    let outcome = reconcile(&anchor, &scan, &confirmed).unwrap();
    assert_recovery_invariants(&outcome);
    assert!(outcome.report.applied.is_empty());
    assert!(outcome.report.chain_suppressed.contains(&first));
    assert!(outcome.report.chain_suppressed.contains(&second));
    drop(pipeline);
    // The chain on disk: founding + one multi-key revision.
    let mut check = LocalFileMedium::open(dir.store()).unwrap();
    assert_eq!(check.validate().unwrap().records, 2);
}

// ----------------------------------------------------------------- tests

/// The W2 window with real process death: the child dies after the
/// T1 boundary transaction, before any live application — the
/// envelope is durable, the anchor lacks the effects, and the
/// independent parent recovers and confirms exactly once.
#[test]
fn crash_after_t1_before_live_application_w2() {
    let dir = TestDir::new("w2");
    let mut child = spawn_child("w2", dir.path());
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "w2 crasher");
    assert!(!status.success(), "the child dies by hard exit: {status:?}");
    reconstruct_confirm_and_verify(&dir, false);
}

/// The W4 window with real process death: the child dies after the
/// live application, before any publication — no durable marker
/// exists (the v1.7 applied-key log is removed by design), so nothing
/// is lost and nothing duplicates; the independent parent re-derives
/// the identical state and confirms exactly once.
#[test]
fn crash_after_live_application_before_publication_w4() {
    let dir = TestDir::new("w4");
    let mut child = spawn_child("w4", dir.path());
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "w4 crasher");
    assert!(!status.success(), "the child dies by hard exit: {status:?}");
    reconstruct_confirm_and_verify(&dir, true);
}
