//! Real-process integration tests — the Batch 2 obligations that
//! in-process mocks cannot establish (the authorization's A–G list):
//!
//! - **A** — multi-writer CAS race: exactly one `Accepted`, losers
//!   `RejectedStaleParent` observing the winner.
//! - **B** — a lock holder killed mid-hold: the OS releases the lock
//!   at process death; `HEAD.lock` is never unlinked; a fresh session
//!   proceeds.
//! - **C** — the crash-window matrix at every storage boundary, with
//!   real child termination (`Abort`) and injected failures
//!   (`InjectError`): the store is always exactly old-or-new — never
//!   torn, never partial.
//! - **D** — an ambiguous acknowledgment (`Unknown`) reconciled by an
//!   independent process and retried idempotently across processes.
//! - **E** — a corrupt-history corpus opened by independent
//!   processes, failing closed.
//! - **F** — repeated crashes with recovery: the chain keeps growing
//!   and every acknowledged revision survives.
//! - **G** — bounded waits: every parental wait carries a deadline
//!   and kills the child on expiry; a deliberately runaway child is
//!   proven killable.
//!
//! Harness: the parent spawns this same test binary with
//! `--exact child_entry` and a role environment; `child_entry` is a
//! no-op in a normal run. Children report outcomes via result files,
//! synchronize on a go-file barrier, and never delete lock files.
//! Blocking OS lock waits are safe by construction (the holder always
//! releases or dies); the harness's own polling waits are the bounded
//! ones.
//!
//! The `HEAD`, `HEAD.lock`, and `objects` names below pin the
//! documented on-disk layout, the same way VCS tests pin their
//! directory names.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use engine_core::Encoder;
use engine_persist::{
    AppendOutcome, AuthorityMode, CanonicalMedium, FaultAction, FaultControl, FaultPoint, Hash,
    LocalFileMedium, Reconciliation, RevisionRecord, StoredObject,
};

const ROLE_ENV: &str = "S7_B2_ROLE";
const ROOT_ENV: &str = "S7_B2_ROOT";
const PARENT_ENV: &str = "S7_B2_PARENT";
const EPOCH_ENV: &str = "S7_B2_EPOCH";
const PAYLOAD_ENV: &str = "S7_B2_PAYLOAD";
const TICK_ENV: &str = "S7_B2_TICK";
const GO_ENV: &str = "S7_B2_GO";
const RESULT_ENV: &str = "S7_B2_RESULT";
const FAULT_ENV: &str = "S7_B2_FAULT";
const ACTION_ENV: &str = "S7_B2_ACTION";
const CANDIDATE_ENV: &str = "S7_B2_CANDIDATE";
const CORRUPT_TRIGGER_ENV: &str = "S7_B2_CORRUPT_GO";
const PROBE_TRIGGER_ENV: &str = "S7_B2_PROBE_GO";
const READY_ENV: &str = "S7_B2_READY";
const CORRUPT_MODE_ENV: &str = "S7_B2_CORRUPT_MODE";
const TARGET_ENV: &str = "S7_B2_TARGET";
const OP_ENV: &str = "S7_B2_OP";

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
            std::env::temp_dir().join(format!("s7-b2-proc-{}-{label}-{seq}", std::process::id()));
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
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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
        "writer" => child_writer(&root_env()),
        "faulted-writer" => child_faulted_writer(&root_env()),
        "reconciler" => child_reconciler(&root_env()),
        "opener" => child_opener(&root_env()),
        "error-holder" => child_error_holder(&root_env()),
        "lock-prober" => child_lock_prober(&root_env()),
        "sleeper" => child_sleeper(),
        other => panic!("unknown role: {other}"),
    }
}

fn root_env() -> PathBuf {
    env::var(ROOT_ENV).expect("root env").into()
}

fn child_writer(root: &Path) {
    wait_for_go();
    let parent = hash_env(PARENT_ENV);
    let epoch = number_env(EPOCH_ENV);
    let tick = number_env(TICK_ENV);
    let payload = env::var(PAYLOAD_ENV).unwrap_or_default();

    let object = content_object(payload.as_bytes());
    let record =
        RevisionRecord::regular(parent, epoch, tick).with_content_refs(vec![object.hash()]);
    let mut medium = LocalFileMedium::open(root).expect("writer open");
    let result = match medium
        .append(&record, &[object], Some(parent))
        .expect("writer append")
    {
        AppendOutcome::Accepted { id, .. } => format!("accepted:{}", id.hex()),
        AppendOutcome::RejectedStaleParent { observed } => {
            format!("stale:{}", hex_or_none(observed))
        }
        AppendOutcome::Unknown { .. } => "unknown".to_owned(),
    };
    write_result(&result);
}

fn child_faulted_writer(root: &Path) {
    wait_for_go();
    let point =
        FaultPoint::from_name(&env::var(FAULT_ENV).expect("fault env")).expect("fault name");
    let action = match env::var(ACTION_ENV).expect("action env").as_str() {
        "abort" => FaultAction::Abort,
        "inject" => FaultAction::InjectError,
        other => panic!("unknown action {other}"),
    };
    let parent = hash_env(PARENT_ENV);
    let epoch = number_env(EPOCH_ENV);
    let tick = number_env(TICK_ENV);
    let payload = env::var(PAYLOAD_ENV).unwrap_or_default();

    let object = content_object(payload.as_bytes());
    let record =
        RevisionRecord::regular(parent, epoch, tick).with_content_refs(vec![object.hash()]);
    let mut medium = LocalFileMedium::open_with(root, Some(FaultControl::new().arm(point, action)))
        .expect("faulted open");
    // For `Abort` the append never returns and no result is written —
    // the parent proves termination and store state instead.
    let result = match medium.append(&record, &[object], Some(parent)) {
        Ok(AppendOutcome::Accepted { id, .. }) => format!("accepted:{}", id.hex()),
        Ok(AppendOutcome::RejectedStaleParent { observed }) => {
            format!("stale:{}", hex_or_none(observed))
        }
        Ok(AppendOutcome::Unknown { reason }) => format!("unknown:{reason:?}"),
        Err(error) => format!("error:{error}"),
    };
    write_result(&result);
}

fn child_reconciler(root: &Path) {
    wait_for_go();
    let candidate = hash_env(CANDIDATE_ENV);
    let mut medium = LocalFileMedium::open(root).expect("reconciler open");
    let result = match medium
        .reconcile_unknown(&candidate)
        .expect("reconciler reconcile")
    {
        Reconciliation::Installed { id } => format!("installed:{}", id.hex()),
        Reconciliation::NotInstalled { head } => {
            format!("not-installed:{}", hex_or_none(head))
        }
        Reconciliation::Unverifiable { .. } => "unverifiable".to_owned(),
    };
    write_result(&result);
}

fn child_opener(root: &Path) {
    wait_for_go();
    let result = match LocalFileMedium::open(root) {
        Err(error) => format!("open-error:{error}"),
        Ok(mut medium) => match medium.validate() {
            Ok(summary) => format!("ok:records={}", summary.records),
            Err(error) => format!("validate-error:{error}"),
        },
    };
    write_result(&result);
}

fn child_sleeper() {
    // A deliberately runaway child for the bounded-wait proof (G).
    std::thread::sleep(Duration::from_secs(600));
}

/// The D1 lock-lifecycle holder: opens while the store is valid,
/// signals readiness, corrupts per the phase trigger, calls the
/// erroring operation, reports the error, and then STAYS ALIVE — a
/// leaked lock would persist until the parent kills us, which is
/// exactly what the lock-prober must prove cannot happen.
fn child_error_holder(root: &Path) {
    wait_for_go();
    let mut medium = LocalFileMedium::open(root).expect("holder open");
    write_ready();
    wait_for_phase(CORRUPT_TRIGGER_ENV);
    match env::var(CORRUPT_MODE_ENV).unwrap_or_default().as_str() {
        "head" => flip_byte(&root.join("HEAD")),
        "record" => {
            let target = hash_env(TARGET_ENV);
            flip_byte(&root.join("objects").join(target.hex()));
        }
        other => panic!("unknown corrupt mode {other}"),
    }
    let result = match env::var(OP_ENV).unwrap_or_default().as_str() {
        "head" => match medium.head() {
            Ok(observed) => format!("head-ok:{}", hex_or_none(observed)),
            Err(error) => format!("head-error:{error}"),
        },
        "validate" => match medium.validate() {
            Ok(summary) => format!("validate-ok:records={}", summary.records),
            Err(error) => format!("validate-error:{error}"),
        },
        other => panic!("unknown op {other}"),
    };
    write_result(&result);
    std::thread::sleep(Duration::from_secs(600));
}

/// The D1 lock prober: opens while the store is valid, waits for the
/// probe phase (after the holder's error), then exercises lock-taking
/// operations — completing within the parent's bound proves the
/// holder's error path released the OS lock.
fn child_lock_prober(root: &Path) {
    wait_for_go();
    let mut medium = LocalFileMedium::open(root).expect("prober open");
    write_ready();
    wait_for_phase(PROBE_TRIGGER_ENV);
    let head = match medium.head() {
        Ok(observed) => format!("head-ok:{}", hex_or_none(observed)),
        Err(error) => format!("head-error:{error}"),
    };
    let validation = match medium.validate() {
        Ok(summary) => format!("validate-ok:records={}", summary.records),
        Err(error) => format!("validate-error:{error}"),
    };
    write_result(&format!("{head};{validation}"));
}

fn write_ready() {
    let path = env::var(READY_ENV).expect("ready env");
    fs::write(&path, "ready").expect("write ready marker");
}

fn wait_for_phase(name: &str) {
    let path = env::var(name).expect("phase env");
    let deadline = Instant::now() + WAIT;
    while !Path::new(&path).exists() {
        assert!(
            Instant::now() < deadline,
            "the {name} phase deadline was exceeded"
        );
        std::thread::sleep(POLL);
    }
}

fn number_env(name: &str) -> u64 {
    env::var(name)
        .unwrap_or_default()
        .parse()
        .expect("numeric env")
}

fn hash_env(name: &str) -> Hash {
    Hash::from_hex(&env::var(name).unwrap_or_default()).expect("hash env")
}

fn hex_or_none(hash: Option<Hash>) -> String {
    hash.map(|hash| hash.hex()).unwrap_or_else(|| "none".into())
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

fn content_object(payload: &[u8]) -> StoredObject {
    let mut enc = Encoder::new();
    enc.write_str("sektor7/test/content/1");
    enc.write_bytes(payload);
    StoredObject::new(enc.into_bytes()).expect("well-formed content object")
}

/// The record a writer child will construct — deterministic: the same
/// parent, epoch, tick, and payload produce the same identity.
fn writer_record(
    parent: Hash,
    epoch: u64,
    tick: u64,
    payload: &str,
) -> (RevisionRecord, StoredObject) {
    let object = content_object(payload.as_bytes());
    let record =
        RevisionRecord::regular(parent, epoch, tick).with_content_refs(vec![object.hash()]);
    (record, object)
}

fn accepted_id(
    medium: &mut LocalFileMedium,
    record: &RevisionRecord,
    objects: &[StoredObject],
    expected_parent: Option<Hash>,
) -> Hash {
    match medium
        .append(record, objects, expected_parent)
        .expect("append")
    {
        AppendOutcome::Accepted { id, .. } => id,
        other => panic!("expected acceptance, got {other:?}"),
    }
}

fn found_in_process(store: &Path) -> Hash {
    let mut medium = LocalFileMedium::open(store).expect("open");
    let record = RevisionRecord::founding(AuthorityMode::Standalone, "process-campaign", 0);
    accepted_id(&mut medium, &record, &[], None)
}

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

enum BoundedWait {
    Exited(ExitStatus),
    KilledAtDeadline,
}

fn wait_bounded_or_kill(child: &mut Child, bound: Duration) -> BoundedWait {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(status) = child.try_wait().expect("try wait") {
            return BoundedWait::Exited(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return BoundedWait::KilledAtDeadline;
        }
        std::thread::sleep(POLL);
    }
}

fn wait_bounded(child: &mut Child, label: &str) -> ExitStatus {
    match wait_bounded_or_kill(child, WAIT) {
        BoundedWait::Exited(status) => status,
        BoundedWait::KilledAtDeadline => {
            panic!("{label} exceeded the {WAIT:?} bounded wait — no hang is accepted")
        }
    }
}

fn flip_byte(path: &Path) {
    let mut bytes = fs::read(path).expect("read");
    bytes[0] ^= 0xff;
    fs::write(path, bytes).expect("write back");
}

/// Bounded wait for a harness file (ready markers, phase results) —
/// every parental wait carries a deadline, no hang is accepted.
fn wait_for_file(path: impl AsRef<Path>) {
    let path = path.as_ref();
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "the file {} never appeared within the bound",
            path.display()
        );
        std::thread::sleep(POLL);
    }
}

/// A founding + one regular revision carrying one content object.
fn founded_content_store(store: &Path) -> (Hash, Hash, Hash) {
    let mut medium = LocalFileMedium::open(store).expect("open");
    let founding = RevisionRecord::founding(AuthorityMode::Standalone, "process-campaign", 0);
    let f = accepted_id(&mut medium, &founding, &[], None);
    let object = content_object(b"corpus-content");
    let content_hash = object.hash();
    let record = RevisionRecord::regular(f, 0, 1).with_content_refs(vec![content_hash]);
    let r = accepted_id(&mut medium, &record, &[object], Some(f));
    (f, r, content_hash)
}

// ---------------------------------------------------------------- test A

#[test]
fn a_multi_writer_cas_race_single_winner() {
    let dir = TestDir::new("race");
    let store = dir.store();
    let f = found_in_process(&store);

    const WRITERS: usize = 4;
    let mut children = Vec::new();
    for index in 0..WRITERS {
        children.push(spawn_child(
            "writer",
            &[
                (ROOT_ENV, store.display().to_string()),
                (PARENT_ENV, f.hex()),
                (EPOCH_ENV, "0".into()),
                (PAYLOAD_ENV, format!("race-payload-{index}")),
                (TICK_ENV, "1".into()),
            ],
            dir.path(),
            index,
        ));
    }
    release_barrier(dir.path());

    let mut results = Vec::new();
    for (index, child) in children.iter_mut().enumerate() {
        let status = wait_bounded(child, "race writer");
        assert!(status.success(), "race writer {index} exited: {status:?}");
        results.push(read_result(dir.path(), index));
    }

    let winners: Vec<&String> = results
        .iter()
        .filter(|result| result.starts_with("accepted:"))
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "exactly one writer wins the CAS: {results:?}"
    );
    let winner =
        Hash::from_hex(winners[0].strip_prefix("accepted:").expect("prefix")).expect("winner hex");
    let losers: Vec<&String> = results
        .iter()
        .filter(|result| result.starts_with("stale:"))
        .collect();
    assert_eq!(losers.len(), WRITERS - 1, "the rest are stale: {results:?}");
    for result in losers {
        assert_eq!(
            result.strip_prefix("stale:").expect("prefix"),
            winner.hex(),
            "losers observe the winner's head"
        );
    }

    let mut medium = LocalFileMedium::open(&store).expect("reopen");
    let summary = medium.validate().expect("validate");
    assert_eq!(
        summary.records, 2,
        "the chain is founding + the single winner"
    );
    assert_eq!(summary.head, winner);
}

// ---------------------------------------------------------------- test B

#[test]
fn b_killed_lock_holder_releases_via_os_without_stale_lock_deletion() {
    let dir = TestDir::new("lock-death");
    let store = dir.store();
    let f = found_in_process(&store);
    let lock = store.join("HEAD.lock");
    assert!(lock.exists());

    // The child aborts at after-lock — terminated while genuinely
    // holding the OS lock, mid-append.
    let mut child = spawn_child(
        "faulted-writer",
        &[
            (ROOT_ENV, store.display().to_string()),
            (PARENT_ENV, f.hex()),
            (EPOCH_ENV, "0".into()),
            (PAYLOAD_ENV, "lock-death-payload".into()),
            (TICK_ENV, "1".into()),
            (FAULT_ENV, "after-lock".into()),
            (ACTION_ENV, "abort".into()),
        ],
        dir.path(),
        0,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "lock-holding writer");
    assert!(
        !status.success(),
        "the child must have aborted while holding the lock: {status:?}"
    );
    assert!(lock.exists(), "HEAD.lock is never unlinked");

    // The OS released the lock at process death: a fresh session
    // opens, validates, and appends — nothing was deleted.
    let mut medium = LocalFileMedium::open(&store).expect("open after holder death");
    let summary = medium.validate().expect("validate after holder death");
    assert_eq!(summary.head, f);
    let (record, object) = writer_record(f, 0, 1, "after-lock-death");
    let recovered = accepted_id(&mut medium, &record, &[object], Some(f));
    assert_ne!(recovered, f, "the append after lock release installed");
    assert!(lock.exists(), "HEAD.lock is still never unlinked");
}

// ---------------------------------------------------------------- test C

/// The append-path matrix: (fault name, injected classification).
/// `error` marks pre-window points — nothing has been written, so an
/// injected failure is a clean error, never an `Unknown` outcome.
const APPEND_FAULTS: &[(&str, &str)] = &[
    ("before-lock", "error"),
    ("after-lock", "error"),
    ("after-validation", "error"),
    ("after-comparison", "error"),
    ("mid-object-write", "unknown:ObjectWriteFailed"),
    ("objects-written", "unknown:ObjectWriteFailed"),
    ("objects-synced", "unknown:ObjectSyncFailed"),
    ("after-objects-dir-sync", "unknown:ObjectSyncFailed"),
    ("head-tmp-written", "unknown:HeadPublishFailed"),
    ("head-tmp-synced", "unknown:HeadPublishFailed"),
    ("after-head-rename", "unknown:PostPublishSyncFailed"),
    ("after-root-dir-sync", "unknown:PostPublishSyncFailed"),
];

/// The rename boundary: points at or before `head-tmp-synced` leave
/// the old HEAD; the two post-rename points have installed.
fn installs_head(fault: &str) -> bool {
    fault == "after-head-rename" || fault == "after-root-dir-sync"
}

#[test]
fn c_crash_window_matrix_old_or_new_never_torn() {
    for (fault, injected) in APPEND_FAULTS {
        for action in ["abort", "inject"] {
            let dir = TestDir::new(&format!("matrix-{fault}-{action}"));
            let store = dir.store();
            let f = found_in_process(&store);
            let (record, _object) = writer_record(f, 0, 1, "matrix-payload");
            let w = record.id();

            let mut child = spawn_child(
                "faulted-writer",
                &[
                    (ROOT_ENV, store.display().to_string()),
                    (PARENT_ENV, f.hex()),
                    (EPOCH_ENV, "0".into()),
                    (PAYLOAD_ENV, "matrix-payload".into()),
                    (TICK_ENV, "1".into()),
                    (FAULT_ENV, (*fault).to_owned()),
                    (ACTION_ENV, (*action).to_owned()),
                ],
                dir.path(),
                0,
            );
            release_barrier(dir.path());
            let label = format!("{fault}/{action} writer");
            let status = wait_bounded(&mut child, &label);

            match action {
                "abort" => {
                    assert!(
                        !status.success(),
                        "{label}: the child must terminate at the boundary: {status:?}"
                    );
                }
                "inject" => {
                    assert!(
                        status.success(),
                        "{label}: the injected child exits cleanly: {status:?}"
                    );
                    let result = read_result(dir.path(), 0);
                    if *injected == "error" {
                        assert!(
                            result.starts_with("error:"),
                            "{label}: expected a clean error, got {result}"
                        );
                    } else {
                        assert_eq!(
                            result, *injected,
                            "{label}: the classification must be honest"
                        );
                    }
                }
                _ => unreachable!(),
            }

            // The store opens and validates: HEAD is exactly the old
            // or the new revision — never torn, never partial.
            let mut medium = LocalFileMedium::open(&store)
                .unwrap_or_else(|_| panic!("{label}: the store must open after the fault"));
            let summary = medium
                .validate()
                .unwrap_or_else(|_| panic!("{label}: the store must validate after the fault"));
            if installs_head(fault) {
                assert_eq!(
                    summary.head, w,
                    "{label}: the rename installed the revision"
                );
                assert_eq!(summary.records, 2, "{label}: founding + installed");
            } else {
                assert_eq!(summary.head, f, "{label}: HEAD keeps the old revision");
                assert_eq!(summary.records, 1, "{label}: only the founding remains");
            }
            assert!(
                store.join("HEAD.lock").exists(),
                "{label}: the lock file is never unlinked"
            );
        }
    }
}

// ---------------------------------------------------------------- test D

#[test]
fn d_ambiguous_acknowledgment_after_rename_is_installed() {
    let dir = TestDir::new("ambiguous-installed");
    let store = dir.store();
    let f = found_in_process(&store);
    let (record, _object) = writer_record(f, 0, 1, "ambiguous-installed");
    let w = record.id();

    // The writer fails after the rename: Unknown, but installed.
    let mut child = spawn_child(
        "faulted-writer",
        &[
            (ROOT_ENV, store.display().to_string()),
            (PARENT_ENV, f.hex()),
            (EPOCH_ENV, "0".into()),
            (PAYLOAD_ENV, "ambiguous-installed".into()),
            (TICK_ENV, "1".into()),
            (FAULT_ENV, "after-head-rename".into()),
            (ACTION_ENV, "inject".into()),
        ],
        dir.path(),
        0,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "ambiguous writer");
    assert!(status.success());
    assert_eq!(
        read_result(dir.path(), 0),
        "unknown:PostPublishSyncFailed",
        "the acknowledgment is honestly uncertain"
    );

    // An independent process reconciles: installed.
    let mut reconciler = spawn_child(
        "reconciler",
        &[
            (ROOT_ENV, store.display().to_string()),
            (CANDIDATE_ENV, w.hex()),
        ],
        dir.path(),
        1,
    );
    let status = wait_bounded(&mut reconciler, "reconciler");
    assert!(status.success());
    assert_eq!(read_result(dir.path(), 1), format!("installed:{}", w.hex()));

    // An idempotent retry with the same record bytes must not
    // duplicate the effect.
    let mut retry = spawn_child(
        "writer",
        &[
            (ROOT_ENV, store.display().to_string()),
            (PARENT_ENV, f.hex()),
            (EPOCH_ENV, "0".into()),
            (PAYLOAD_ENV, "ambiguous-installed".into()),
            (TICK_ENV, "1".into()),
        ],
        dir.path(),
        2,
    );
    let status = wait_bounded(&mut retry, "retry writer");
    assert!(status.success());
    assert_eq!(read_result(dir.path(), 2), format!("stale:{}", w.hex()));

    let mut medium = LocalFileMedium::open(&store).expect("reopen");
    let summary = medium.validate().expect("validate");
    assert_eq!(summary.records, 2, "no duplication across the retry");
    assert_eq!(summary.head, w);
}

#[test]
fn d_ambiguous_acknowledgment_before_rename_retries_idempotently() {
    let dir = TestDir::new("ambiguous-not-installed");
    let store = dir.store();
    let f = found_in_process(&store);
    let (record, _object) = writer_record(f, 0, 1, "ambiguous-retry");
    let w = record.id();

    // The writer fails before the rename: Unknown, not installed.
    let mut child = spawn_child(
        "faulted-writer",
        &[
            (ROOT_ENV, store.display().to_string()),
            (PARENT_ENV, f.hex()),
            (EPOCH_ENV, "0".into()),
            (PAYLOAD_ENV, "ambiguous-retry".into()),
            (TICK_ENV, "1".into()),
            (FAULT_ENV, "head-tmp-synced".into()),
            (ACTION_ENV, "inject".into()),
        ],
        dir.path(),
        0,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "ambiguous writer");
    assert!(status.success());
    assert_eq!(
        read_result(dir.path(), 0),
        "unknown:HeadPublishFailed",
        "the acknowledgment is honestly uncertain"
    );

    // An independent process reconciles: not installed (its
    // session-open sweep quarantines the unreferenced candidate
    // objects; reconcile reports retry-safe absence).
    let mut reconciler = spawn_child(
        "reconciler",
        &[
            (ROOT_ENV, store.display().to_string()),
            (CANDIDATE_ENV, w.hex()),
        ],
        dir.path(),
        1,
    );
    let status = wait_bounded(&mut reconciler, "reconciler");
    assert!(status.success());
    assert_eq!(
        read_result(dir.path(), 1),
        format!("not-installed:{}", f.hex())
    );

    // The retry with the same record bytes installs exactly once.
    let mut retry = spawn_child(
        "writer",
        &[
            (ROOT_ENV, store.display().to_string()),
            (PARENT_ENV, f.hex()),
            (EPOCH_ENV, "0".into()),
            (PAYLOAD_ENV, "ambiguous-retry".into()),
            (TICK_ENV, "1".into()),
        ],
        dir.path(),
        2,
    );
    let status = wait_bounded(&mut retry, "retry writer");
    assert!(status.success());
    assert_eq!(read_result(dir.path(), 2), format!("accepted:{}", w.hex()));

    // Reconciling the same candidate again now classifies it as
    // installed.
    let mut again = spawn_child(
        "reconciler",
        &[
            (ROOT_ENV, store.display().to_string()),
            (CANDIDATE_ENV, w.hex()),
        ],
        dir.path(),
        3,
    );
    let status = wait_bounded(&mut again, "re-reconciler");
    assert!(status.success());
    assert_eq!(read_result(dir.path(), 3), format!("installed:{}", w.hex()));

    let mut medium = LocalFileMedium::open(&store).expect("reopen");
    let summary = medium.validate().expect("validate");
    assert_eq!(summary.records, 2, "the retry installed exactly once");
    assert_eq!(summary.head, w);
}

// ---------------------------------------------------------------- test E

#[test]
fn e_corrupt_history_corpus_fails_closed_across_processes() {
    // (a) A torn HEAD.
    {
        let dir = TestDir::new("corpus-head");
        let store = dir.store();
        founded_content_store(&store);
        let head = store.join("HEAD");
        let bytes = fs::read(&head).expect("read HEAD");
        fs::write(&head, &bytes[..10]).expect("tear HEAD");
        run_opener_expect(&dir, &store, "open-error", "authoritative HEAD is corrupt");
    }
    // (b) A chain-referenced record with flipped bytes.
    {
        let dir = TestDir::new("corpus-record");
        let store = dir.store();
        let (f, _r, _content) = founded_content_store(&store);
        flip_byte(&store.join("objects").join(f.hex()));
        run_opener_expect(&dir, &store, "open-error", "chain-referenced object");
    }
    // (c) HEAD names a missing object.
    {
        let dir = TestDir::new("corpus-missing");
        let store = dir.store();
        let (_f, r, _content) = founded_content_store(&store);
        fs::remove_file(store.join("objects").join(r.hex())).expect("remove head object");
        run_opener_expect(&dir, &store, "open-error", "is missing from the store");
    }
    // (d) A chain-referenced content object with flipped bytes.
    {
        let dir = TestDir::new("corpus-content");
        let store = dir.store();
        let (_f, _r, content) = founded_content_store(&store);
        flip_byte(&store.join("objects").join(content.hex()));
        run_opener_expect(&dir, &store, "open-error", "chain-referenced object");
    }
}

fn run_opener_expect(dir: &TestDir, store: &Path, prefix: &str, needle: &str) {
    let mut child = spawn_child(
        "opener",
        &[(ROOT_ENV, store.display().to_string())],
        dir.path(),
        0,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "corpus opener");
    assert!(
        status.success(),
        "the opener reports its failure via result"
    );
    let result = read_result(dir.path(), 0);
    assert!(result.starts_with(prefix), "got: {result}");
    assert!(result.contains(needle), "got: {result}");
}

// ---------------------------------------------------------------- test F

#[test]
fn f_repeated_crash_recovery_grows_a_valid_chain() {
    let dir = TestDir::new("repeat-crash");
    let store = dir.store();
    let mut head = found_in_process(&store);
    let mut epoch = 0u64;
    let mut expected_records = 1u64;

    // Rotate crash boundaries across the uncertain window and past
    // the rename: every crash must leave old-or-new, and the chain
    // keeps growing.
    let crashes = [
        "mid-object-write",
        "objects-written",
        "objects-synced",
        "after-objects-dir-sync",
        "head-tmp-written",
        "head-tmp-synced",
        "after-head-rename",
        "after-root-dir-sync",
    ];
    for (round, fault) in crashes.iter().enumerate() {
        let payload = format!("crash-round-{round}");
        let tick = round as u64 + 1;
        let (record, _object) = writer_record(head, epoch, tick, &payload);
        let crashed_id = record.id();

        let mut child = spawn_child(
            "faulted-writer",
            &[
                (ROOT_ENV, store.display().to_string()),
                (PARENT_ENV, head.hex()),
                (EPOCH_ENV, epoch.to_string()),
                (PAYLOAD_ENV, payload),
                (TICK_ENV, tick.to_string()),
                (FAULT_ENV, (*fault).to_owned()),
                (ACTION_ENV, "abort".into()),
            ],
            dir.path(),
            round,
        );
        release_barrier(dir.path());
        let status = wait_bounded(&mut child, "crashing writer");
        assert!(
            !status.success(),
            "round {round}: the child must abort at {fault}: {status:?}"
        );

        // Recovery: open and validate without hanging — old or new.
        let mut medium = LocalFileMedium::open(&store)
            .unwrap_or_else(|_| panic!("round {round}: open after crash"));
        let summary = medium
            .validate()
            .unwrap_or_else(|_| panic!("round {round}: validate after crash"));
        assert!(
            summary.head == head || summary.head == crashed_id,
            "round {round}: HEAD must be old-or-new, got {}",
            summary.head
        );
        if summary.head == crashed_id {
            head = crashed_id;
            epoch += 1;
            expected_records += 1;
        }

        // The chain keeps growing after every crash.
        let payload = format!("recovery-round-{round}");
        let (record, object) = writer_record(head, epoch, 100 + round as u64, &payload);
        head = accepted_id(&mut medium, &record, &[object], Some(head));
        epoch += 1;
        expected_records += 1;
    }

    let mut medium = LocalFileMedium::open(&store).expect("final open");
    let summary = medium.validate().expect("final validate");
    assert_eq!(summary.records, expected_records);
    assert_eq!(summary.head, head);
    assert_eq!(summary.height, epoch);
    assert!(store.join("HEAD.lock").exists());
}

// ---------------------------------------------------------------- test G

#[test]
fn g_bounded_waits_kill_runaway_children() {
    let dir = TestDir::new("runaway");
    let mut child = spawn_child("sleeper", &[], dir.path(), 0);
    release_barrier(dir.path());
    match wait_bounded_or_kill(&mut child, Duration::from_secs(2)) {
        BoundedWait::KilledAtDeadline => {}
        BoundedWait::Exited(status) => {
            panic!("the sleeper should have outlived the bound, exited: {status:?}")
        }
    }
}

// -------------------------------------- review corrective B + D1 (cross-process)

fn run_writer_expect_accepted(
    dir: &TestDir,
    store: &Path,
    parent: Hash,
    epoch: u64,
    payload: &str,
    tick: u64,
    index: usize,
) {
    let mut child = spawn_child(
        "writer",
        &[
            (ROOT_ENV, store.display().to_string()),
            (PARENT_ENV, parent.hex()),
            (EPOCH_ENV, epoch.to_string()),
            (PAYLOAD_ENV, payload.to_owned()),
            (TICK_ENV, tick.to_string()),
        ],
        dir.path(),
        index,
    );
    release_barrier(dir.path());
    let status = wait_bounded(&mut child, "external writer");
    assert!(status.success(), "the writer exited: {status:?}");
    let result = read_result(dir.path(), index);
    assert!(result.starts_with("accepted:"), "got: {result}");
}

/// B: another process appends two revisions before the stale session
/// observes the new HEAD — the stale session absorbs the complete
/// suffix, stale-rejects against the winner, and then appends a valid
/// direct descendant with no further walk.
#[test]
fn b_external_multi_revision_suffix_absorbed_cross_process() {
    let dir = TestDir::new("b-cross-suffix");
    let store = dir.store();
    let f = found_in_process(&store);
    // The stale session, opened before the external writers append.
    let mut stale = LocalFileMedium::open(&store).expect("stale open");

    // Writer 1 appends the first external revision.
    let (w1_record, _w1_object) = writer_record(f, 0, 1, "b-cross-one");
    let w1 = w1_record.id();
    run_writer_expect_accepted(&dir, &store, f, 0, "b-cross-one", 1, 0);

    // Writer 2 appends the second external revision on top of w1.
    let (w2_record, _w2_object) = writer_record(w1, 1, 2, "b-cross-two");
    let w2 = w2_record.id();
    run_writer_expect_accepted(&dir, &store, w1, 1, "b-cross-two", 2, 1);

    // The stale session still expects f: the complete two-record
    // suffix is validated and absorbed; the append is stale-rejected
    // with the winner observed.
    let (stale_record, stale_object) = writer_record(f, 0, 3, "b-cross-stale");
    match stale
        .append(&stale_record, std::slice::from_ref(&stale_object), Some(f))
        .expect("stale append completes")
    {
        AppendOutcome::RejectedStaleParent { observed } => assert_eq!(observed, Some(w2)),
        other => panic!("expected stale rejection, got {other:?}"),
    }
    assert_eq!(
        stale.walk_stats().last_append_walked,
        2,
        "the complete two-record suffix was walked"
    );

    // The absorbed cursor is consistent: a direct descendant of w2 at
    // epoch 3 is accepted with no further walk.
    let (next_record, next_object) = writer_record(w2, 2, 4, "b-cross-next");
    let next_id = accepted_id(
        &mut stale,
        &next_record,
        std::slice::from_ref(&next_object),
        Some(w2),
    );
    assert_eq!(next_id, next_record.id());
    assert_eq!(
        stale.walk_stats().last_append_walked,
        0,
        "the next ordinary append requires no additional history walk"
    );

    let summary = stale.validate().expect("validate");
    assert_eq!(summary.records, 4);
    assert_eq!(summary.height, 3);
    assert_eq!(summary.head, next_id);
}

/// D1: `head()`'s error path must release HEAD.lock while the medium
/// stays alive — a separate process (its medium already open from
/// before the corruption) acquires the lock and completes within the
/// bound. Corrupt-HEAD variant.
#[test]
fn d1_lock_released_after_head_error_cross_process() {
    let dir = TestDir::new("d1-head-error");
    let store = dir.store();
    founded_content_store(&store);
    let harness = dir.path();

    let mut holder = spawn_child(
        "error-holder",
        &[
            (ROOT_ENV, store.display().to_string()),
            (
                CORRUPT_TRIGGER_ENV,
                harness.join("corrupt-go").display().to_string(),
            ),
            (
                READY_ENV,
                harness.join("ready-holder").display().to_string(),
            ),
            (CORRUPT_MODE_ENV, "head".into()),
            (OP_ENV, "head".into()),
        ],
        harness,
        0,
    );
    let mut prober = spawn_child(
        "lock-prober",
        &[
            (ROOT_ENV, store.display().to_string()),
            (
                PROBE_TRIGGER_ENV,
                harness.join("probe-go").display().to_string(),
            ),
            (
                READY_ENV,
                harness.join("ready-prober").display().to_string(),
            ),
        ],
        harness,
        1,
    );

    // Both children open while the store is valid.
    release_barrier(harness);
    wait_for_file(harness.join("ready-holder"));
    wait_for_file(harness.join("ready-prober"));

    // The holder corrupts HEAD and calls head() — the error path runs
    // while the medium stays alive.
    fs::write(harness.join("corrupt-go"), b"go").expect("release the corrupt phase");
    wait_for_file(harness.join("result-0"));
    let holder_result = read_result(harness, 0);
    assert!(
        holder_result.starts_with("head-error:"),
        "the holder must report the corrupt-HEAD error: {holder_result}"
    );
    assert!(holder_result.contains("authoritative HEAD is corrupt"));

    // The prober must acquire the lock and complete within the bound;
    // a leaked lock would block it forever and fail the bounded wait.
    fs::write(harness.join("probe-go"), b"go").expect("release the probe phase");
    let status = wait_bounded(&mut prober, "lock prober");
    assert!(status.success(), "the prober exited: {status:?}");
    let prober_result = read_result(harness, 1);
    assert!(
        prober_result.starts_with("head-error:"),
        "the prober's head() must fail closed on the corrupt HEAD: {prober_result}"
    );
    assert!(
        prober_result.contains("validate-error:"),
        "the prober's validate() must fail closed too: {prober_result}"
    );

    // The holder sleeps by design; kill it.
    let _ = holder.kill();
    let _ = holder.wait();
}

/// D1: `validate()`'s error path must release HEAD.lock while the
/// medium stays alive — failed-validation variant (a corrupted record
/// object, HEAD itself intact).
#[test]
fn d1_lock_released_after_validation_error_cross_process() {
    let dir = TestDir::new("d1-validation-error");
    let store = dir.store();
    let (_f, r, _content) = founded_content_store(&store);
    let harness = dir.path();

    let mut holder = spawn_child(
        "error-holder",
        &[
            (ROOT_ENV, store.display().to_string()),
            (
                CORRUPT_TRIGGER_ENV,
                harness.join("corrupt-go").display().to_string(),
            ),
            (
                READY_ENV,
                harness.join("ready-holder").display().to_string(),
            ),
            (CORRUPT_MODE_ENV, "record".into()),
            (TARGET_ENV, r.hex()),
            (OP_ENV, "validate".into()),
        ],
        harness,
        0,
    );
    let mut prober = spawn_child(
        "lock-prober",
        &[
            (ROOT_ENV, store.display().to_string()),
            (
                PROBE_TRIGGER_ENV,
                harness.join("probe-go").display().to_string(),
            ),
            (
                READY_ENV,
                harness.join("ready-prober").display().to_string(),
            ),
        ],
        harness,
        1,
    );

    release_barrier(harness);
    wait_for_file(harness.join("ready-holder"));
    wait_for_file(harness.join("ready-prober"));

    fs::write(harness.join("corrupt-go"), b"go").expect("release the corrupt phase");
    wait_for_file(harness.join("result-0"));
    let holder_result = read_result(harness, 0);
    assert!(
        holder_result.starts_with("validate-error:"),
        "the holder must report the validation error: {holder_result}"
    );
    assert!(holder_result.contains("chain-referenced object"));

    fs::write(harness.join("probe-go"), b"go").expect("release the probe phase");
    let status = wait_bounded(&mut prober, "lock prober");
    assert!(status.success(), "the prober exited: {status:?}");
    let prober_result = read_result(harness, 1);
    assert!(
        prober_result.starts_with("head-ok:"),
        "the prober's head() succeeds on the valid HEAD (the cursor path): {prober_result}"
    );
    assert!(
        prober_result.contains("validate-error:"),
        "the prober's validate() must fail closed on the corrupted record: {prober_result}"
    );

    let _ = holder.kill();
    let _ = holder.wait();
}
