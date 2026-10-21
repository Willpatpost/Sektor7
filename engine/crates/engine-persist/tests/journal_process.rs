//! Real-process integration tests for the durable T1 journal — the
//! journal's crash matrix (the review's requirement: where the
//! contract requires process-crash evidence, use real child-process
//! failure tests rather than relying exclusively on simulated
//! errors).
//!
//! - **Ack survival** — transactions acknowledged in one process are
//!   committed and actionable in a fresh process; the acknowledgment
//!   bit is never fabricated across the process boundary.
//! - **Crash after sync, before ack** — the child dies after the
//!   fsync but before the receipt returns: the transaction is durable
//!   and recoverable, and the new process reports it `Committed`,
//!   never `AcknowledgedT1` (no invented historical acknowledgment).
//! - **Crash mid-capture** — the child dies during the capture
//!   frames: an inert tail (incomplete preparation, never replayed);
//!   a fresh process resolves it and the retry commits exactly once
//!   under the same identity.
//! - **Ambiguous commit, reconciled across processes** — the child
//!   observes and reports the ambiguous commit-phase failure; an
//!   independent process reconciles by rereading and verifying:
//!   `Committed`, actionable, without a fabricated acknowledgment.
//!
//! Harness: the parent spawns this same test binary with `--exact
//! child_entry` and a role environment (the Batch 2 pattern); the
//! child synchronizes on a go-file barrier and reports outcomes via
//! result files; every parental wait carries a deadline.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use engine_persist::{
    AwardCheckpoint, ClassifiedSpan, ConsequenceEnvelope, ConsequenceId, CounterValues, Coverage,
    DurabilityClass, FaultAction, FrontierEntry, Hash, JournalError, JournalFaultControl,
    JournalFaultPoint, OccurrenceSpan, SignalLane, SourceBaseline, SourcePosition, T1Journal,
    TailState, TransactionDurability, TransactionId, TransactionInput, WriteReconciliation,
};

const ROLE_ENV: &str = "S7_J3_ROLE";
const PATH_ENV: &str = "S7_J3_PATH";
const FAULT_ENV: &str = "S7_J3_FAULT";
const ACTION_ENV: &str = "S7_J3_ACTION";
const COUNT_ENV: &str = "S7_J3_COUNT";
const GO_ENV: &str = "S7_J3_GO";
const RESULT_ENV: &str = "S7_J3_RESULT";

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
            std::env::temp_dir().join(format!("s7-j3-proc-{}-{label}-{seq}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test root");
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
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

/// The deterministic transaction a writer child (or the parent)
/// constructs — the same index produces the same stable identity.
fn tx_input(index: u64) -> TransactionInput {
    let identity = ConsequenceId::new(1, index, 1);
    let envelope = ConsequenceEnvelope::capture(
        identity,
        vec![ClassifiedSpan {
            span: OccurrenceSpan::new(2 * (index - 1), 2 * index).unwrap(),
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
            tick: 100 + index,
        },
    )
    .unwrap();
    TransactionInput {
        envelopes: vec![envelope],
        position: SourcePosition {
            head: None,
            tick: 100 + index,
        },
        classification: DurabilityClass::DurableEligible,
        dependencies: Vec::new(),
        frontier: vec![FrontierEntry {
            region: 1,
            episode: index,
        }],
        plan: format!("plan-{index}").into_bytes(),
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
        "jwriter" => child_writer(),
        other => panic!("unknown role: {other}"),
    }
}

/// Appends `COUNT` transactions successfully, then — if a fault is
/// armed — appends one more transaction through the faulted journal.
/// The prefix journal is dropped before the faulted open: the
/// journal's exclusive OS lock is per open-file-description, so a
/// same-process second open would block.
fn child_writer() {
    wait_for_go();
    let path = PathBuf::from(env::var(PATH_ENV).expect("path env"));
    let count = number_env(COUNT_ENV);

    let mut journal = T1Journal::open(&path).expect("writer open");
    let mut lines = Vec::new();
    for index in 1..=count {
        let receipt = journal
            .append_transaction(&tx_input(index))
            .expect("writer append");
        lines.push(format!("ok:{}:{}", receipt.id.0.hex(), receipt.sequence));
    }
    drop(journal);

    if let (Some(point), Some(action)) = (
        env::var(FAULT_ENV)
            .ok()
            .and_then(|name| JournalFaultPoint::from_name(&name)),
        env::var(ACTION_ENV)
            .ok()
            .and_then(|name| match name.as_str() {
                "abort" => Some(FaultAction::Abort),
                "inject" => Some(FaultAction::InjectError),
                _ => None,
            }),
    ) {
        let control = JournalFaultControl::new().arm(point, action);
        let mut faulted = T1Journal::open_with(&path, Some(control)).expect("faulted open");
        // For `Abort` the append never returns and no result is
        // written — the parent proves termination and the journal's
        // state instead.
        match faulted.append_transaction(&tx_input(count + 1)) {
            Ok(receipt) => {
                lines.push(format!("ok-after-fault:{}", receipt.id.0.hex()));
            }
            Err(JournalError::AbortedBeforeCommit { .. }) => lines.push(format!(
                "aborted:{}",
                tx_input(count + 1).transaction_id().unwrap().0.hex()
            )),
            Err(JournalError::AmbiguousCommit { id, .. }) => {
                lines.push(format!("ambiguous:{}", id.0.hex()));
            }
            Err(error) => lines.push(format!("error:{error}")),
        }
    }
    write_result(&lines.join("\n"));
}

fn number_env(name: &str) -> u64 {
    env::var(name)
        .unwrap_or_default()
        .parse()
        .expect("numeric env")
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

fn spawn_child(role: &str, envs: &[(&str, String)], harness: &Path, index: usize) -> Child {
    let go = harness.join("go");
    let result = harness.join(format!("result-{index}"));
    let mut command = Command::new(env::current_exe().expect("current exe"));
    command
        .arg("--exact")
        .arg("child_entry")
        .env(ROLE_ENV, role)
        .env(GO_ENV, go.display().to_string())
        .env(RESULT_ENV, result.display().to_string());
    for (key, value) in envs {
        command.env(key, value);
    }
    command.stdout(Stdio::null()).stderr(Stdio::null());
    command.spawn().expect("spawn child")
}

fn release_barrier(harness: &Path) {
    fs::write(harness.join("go"), b"go").expect("release the go barrier");
}

fn read_result(harness: &Path, index: usize) -> String {
    fs::read_to_string(harness.join(format!("result-{index}"))).expect("read result file")
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

// ----------------------------------------------------------------- tests

#[test]
fn acked_transactions_survive_process_death() {
    let dir = TestDir::new("survive");
    let path = dir.journal();
    let mut child = spawn_child(
        "jwriter",
        &[
            (PATH_ENV, path.display().to_string()),
            (COUNT_ENV, "3".into()),
        ],
        dir.path(),
        0,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "surviving writer");
    assert!(status.success(), "the writer exits cleanly: {status:?}");
    let result = read_result(dir.path(), 0);
    let lines: Vec<&str> = result.lines().collect();
    assert_eq!(lines.len(), 3, "three acknowledged receipts: {result}");

    // A fresh process observes every acknowledged transaction as
    // committed and actionable — the acknowledgment itself is never
    // fabricated across the boundary.
    let journal = T1Journal::open(&path).unwrap();
    assert_eq!(journal.transactions().len(), 3);
    for line in &lines {
        let hex = line
            .strip_prefix("ok:")
            .expect("ok prefix")
            .split(':')
            .next()
            .expect("receipt hex");
        let id = TransactionId(Hash::from_hex(hex).expect("receipt hex"));
        assert_eq!(
            journal.transaction_status(id),
            Some(TransactionDurability::Committed)
        );
        assert!(!journal.acknowledged_this_session(id));
        assert!(journal.is_actionable(id));
    }
    assert_eq!(journal.next_sequence(), 4);
}

#[test]
fn crash_after_sync_before_ack_recovers_committed_without_fabricated_ack() {
    let dir = TestDir::new("after-sync");
    let path = dir.journal();
    let mut child = spawn_child(
        "jwriter",
        &[
            (PATH_ENV, path.display().to_string()),
            (COUNT_ENV, "2".into()),
            (FAULT_ENV, "after-sync".into()),
            (ACTION_ENV, "abort".into()),
        ],
        dir.path(),
        0,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "after-sync crasher");
    assert!(
        !status.success(),
        "the child must die after the sync, before the receipt: {status:?}"
    );

    // The third transaction IS durable (the sync completed) even
    // though its caller never received the receipt: committed and
    // actionable in the fresh process, never AcknowledgedT1 — the
    // four-state distinction proven across a real process death.
    let journal = T1Journal::open(&path).unwrap();
    assert_eq!(
        journal.transactions().len(),
        3,
        "two reported + one durable-unreported"
    );
    let third = tx_input(3).transaction_id().unwrap();
    assert_eq!(
        journal.transaction_status(third),
        Some(TransactionDurability::Committed)
    );
    assert!(!journal.acknowledged_this_session(third));
    assert!(journal.is_actionable(third));
    assert_eq!(journal.next_sequence(), 4);
}

#[test]
fn crash_mid_capture_leaves_inert_tail_then_clean_retry() {
    let dir = TestDir::new("mid-capture");
    let path = dir.journal();
    let mut child = spawn_child(
        "jwriter",
        &[
            (PATH_ENV, path.display().to_string()),
            (COUNT_ENV, "1".into()),
            (FAULT_ENV, "mid-capture-write".into()),
            (ACTION_ENV, "abort".into()),
        ],
        dir.path(),
        0,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "mid-capture crasher");
    assert!(!status.success(), "the child dies mid-capture: {status:?}");

    // The aborted attempt is the inert tail — incomplete preparation,
    // never replayed; the acknowledged first transaction survives.
    let scan = T1Journal::scan_file(&path).unwrap();
    assert_eq!(scan.committed.len(), 1);
    assert!(matches!(scan.tail, TailState::Inert { .. }));

    // A fresh process resolves the tail and the retry commits exactly
    // once under the same identity.
    let mut journal = T1Journal::open(&path).unwrap();
    assert_eq!(journal.transactions().len(), 1);
    let receipt = journal.append_transaction(&tx_input(2)).unwrap();
    assert_eq!(receipt.id, tx_input(2).transaction_id().unwrap());
    assert_eq!(journal.transactions().len(), 2, "no duplicate");
    let rescan = T1Journal::scan_file(&path).unwrap();
    assert_eq!(rescan.committed.len(), 2);
    assert_eq!(rescan.tail, TailState::Clean);
}

#[test]
fn ambiguous_commit_reconciles_committed_across_processes() {
    let dir = TestDir::new("ambiguous");
    let path = dir.journal();
    let mut child = spawn_child(
        "jwriter",
        &[
            (PATH_ENV, path.display().to_string()),
            (COUNT_ENV, "1".into()),
            (FAULT_ENV, "commit-written".into()),
            (ACTION_ENV, "inject".into()),
        ],
        dir.path(),
        0,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "ambiguous writer");
    assert!(
        status.success(),
        "the injected failure exits cleanly: {status:?}"
    );
    let result = read_result(dir.path(), 0);
    let expected = format!(
        "ambiguous:{}",
        tx_input(2).transaction_id().unwrap().0.hex()
    );
    assert_eq!(
        result.lines().nth(1),
        Some(expected.as_str()),
        "the child observed the ambiguity: {result}"
    );

    // An independent process reconciles by rereading and verifying:
    // the frame is present and valid — committed, actionable, and NO
    // fabricated acknowledgment.
    let mut journal = T1Journal::open(&path).unwrap();
    let outcome = journal.reconcile_write(&tx_input(2)).unwrap();
    let WriteReconciliation::Committed { receipt } = outcome else {
        panic!("expected Committed, got {outcome:?}");
    };
    assert_eq!(receipt.id, tx_input(2).transaction_id().unwrap());
    assert_eq!(journal.transactions().len(), 2);
    assert!(!journal.acknowledged_this_session(receipt.id));
    assert!(journal.is_actionable(receipt.id));
}
