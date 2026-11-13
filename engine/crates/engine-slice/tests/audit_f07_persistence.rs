//! (Audit F-07) The persistence-throughput benchmark harness —
//! the independent quality/performance audit's assignment (the
//! owner's standing delegation, 2026-11-13): "benchmark the actual
//! worker/SQLite/pipeline workload under controlled contention and
//! crash-safe durability; characterize throughput, queue latency,
//! WAL size, memory and recovery. Propose only semantics-preserving
//! optimizations."
//!
//! **Methodology.** The production topology is measured as it
//! stands: one `IoWorker` thread owning the ONE `SessionStore`
//! connection (SQLite, `journal_mode=WAL` + `synchronous=FULL` —
//! the ratified durability contract, verified by the provider at
//! open), the bounded FIFO queue (capacity 64), and the
//! `WorkerMedium` queue proxies. Every proxied operation's
//! wall time (send → reply) decomposes into the admission wait
//! (the bounded `sync_channel` send), the queue wait, and the
//! worker's execution; the DIRECT provider baseline (the same
//! operations against a plain `SqliteMedium`, no queue) isolates
//! the execution component — under `synchronous=FULL` each
//! append's COMMIT is an fsync of the WAL, so the direct
//! per-append distribution IS the commit/fsync distribution. The
//! delta between the proxied and direct distributions is the
//! queue overhead. Nothing here weakens the durability contract
//! or the required-admission blocking queue; the numbers are
//! printed as `[F07]` lines and asserted only for correctness and
//! internal consistency (no flaky performance gates in CI).
//!
//! **Scale.** The committed form is CI-bounded (hundreds of
//! operations, seconds). The deep local runs scale every test's
//! operation count through the `F07_OPS` environment variable
//! (e.g. `F07_OPS=20000 cargo test --release --test
//! audit_f07_persistence -- --nocapture`) — the 1x/10x/100x load
//! characterization in the delivery report comes from those
//! release-mode runs, never from the CI's debug build.

use std::path::PathBuf;
use std::time::Instant;

use engine_persist::{
    AppendOutcome, CanonicalMedium, Hash, RevisionRecord, StoredObject, SyncTier,
};
use engine_slice::store::{MediumSource, StoreLocation};
use engine_slice::worker::IoWorker;

// ------------------------------------------------------------ the fixtures

fn temp_db(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "s7-f07-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn payload_object(payload: &[u8]) -> StoredObject {
    StoredObject::new({
        let mut enc = engine_core::Encoder::new();
        enc.write_str("sektor7/save/1");
        enc.write_bytes(payload);
        enc.into_bytes()
    })
    .unwrap()
}

/// The operation count for one benchmark run: the CI-bounded
/// default, or the `F07_OPS` deep-mode scale.
fn f07_ops(default: usize) -> usize {
    std::env::var("F07_OPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// The worker-backed store (the production topology: one thread,
/// one connection, the bounded queue).
fn spawn_worker(db: &std::path::Path) -> IoWorker {
    IoWorker::spawn(&StoreLocation::Database(db.to_path_buf())).unwrap()
}

// ------------------------------------------------------------- the stats

/// One latency sample set (microseconds) with the distribution
/// facts the audit asked for.
struct Samples {
    micros: Vec<u128>,
}

impl Samples {
    fn record(&mut self, elapsed: Instant) {
        self.micros.push(elapsed.elapsed().as_micros());
    }

    fn sorted(&self) -> Vec<u128> {
        let mut sorted = self.micros.clone();
        sorted.sort_unstable();
        sorted
    }

    fn percentile(&self, fraction: f64) -> u128 {
        let sorted = self.sorted();
        let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    }

    fn count(&self) -> usize {
        self.micros.len()
    }

    fn mean(&self) -> u128 {
        if self.micros.is_empty() {
            return 0;
        }
        self.micros.iter().sum::<u128>() / self.micros.len() as u128
    }

    fn p50(&self) -> u128 {
        self.percentile(0.50)
    }

    fn p95(&self) -> u128 {
        self.percentile(0.95)
    }

    fn p99(&self) -> u128 {
        self.percentile(0.99)
    }

    fn max(&self) -> u128 {
        self.micros.iter().copied().max().unwrap_or(0)
    }

    fn report(&self, label: &str, wall_seconds: f64) {
        println!(
            "[F07] {label}: n={} mean={}us p50={}us p95={}us p99={}us max={}us throughput={:.0}ops/s",
            self.count(),
            self.mean(),
            self.p50(),
            self.p95(),
            self.p99(),
            self.max(),
            self.count() as f64 / wall_seconds,
        );
    }

    /// The correctness-only assertions (never a performance gate).
    fn assert_consistent(&self) {
        assert!(!self.micros.is_empty());
        assert!(self.p50() <= self.p95());
        assert!(self.p95() <= self.p99());
        assert!(self.p99() <= self.max());
    }
}

/// One append through a medium handle (the pipelined form: the
/// previous append's accepted identity is the next parent — no
/// extra head round trip, exactly the production pipeline's
/// pattern).
fn append_one(
    medium: &mut dyn CanonicalMedium,
    shared: &StoredObject,
    parent: Option<Hash>,
    tick: u64,
) -> Hash {
    let head = match parent {
        Some(id) => id,
        None => medium.head().unwrap().unwrap(),
    };
    let epoch = medium.read_record(&head).unwrap().epoch;
    let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(vec![shared.hash()]);
    match medium.append(&record, std::slice::from_ref(shared), Some(head)) {
        Ok(AppendOutcome::Accepted { id, .. }) => id,
        Ok(other) => panic!("the benchmark append must be Accepted: {other:?}"),
        Err(error) => panic!("the benchmark append failed: {error:?}"),
    }
}

fn file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

// ============================================================= the suites

/// (F-07 §1) The sequential baseline: one producer, one queue
/// proxy, the production worker topology end to end. The per-op
/// wall time includes the admission send, the queue wait, and the
/// worker's execution (the commit + fsync under
/// `synchronous=FULL`); the retained sizes (db + wal) are
/// reported at the end.
#[test]
fn f07_sequential_baseline_throughput_and_latency() {
    let db = temp_db("seq");
    let ops = f07_ops(200);
    let worker = spawn_worker(&db);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let shared = payload_object(b"f07-sequential-payload");
    // The founding publication (before the measured loop).
    let founding =
        RevisionRecord::founding(engine_persist::record::AuthorityMode::Standalone, "f07-seq", 0)
            .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let mut parent = Some(founding.id());
    let mut samples = Samples { micros: Vec::with_capacity(ops) };
    let start = Instant::now();
    for tick in 0..ops as u64 {
        let op_start = Instant::now();
        parent = Some(append_one(&mut medium, &shared, parent, tick));
        samples.record(op_start);
    }
    let wall = start.elapsed().as_secs_f64();
    samples.report("sequential-proxied", wall);
    samples.assert_consistent();
    let wal = file_len(&db.with_extension("db-wal"));
    println!(
        "[F07] sequential-proxied retained: db={}B wal={}B ops={}",
        file_len(&db),
        wal,
        ops
    );
    // The tier is the truthful provider-reported one (WAL+FULL).
    assert_eq!(worker.tier(), SyncTier::TransactionalWalFull);
    // The proxy's queue sender keeps the channel open — drop it
    // before the join (the worker exits when the channel closes).
    drop(medium);
    worker.join().unwrap();
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
}

/// (F-07 §2) The direct-provider baseline: the SAME append
/// workload against a plain `SqliteMedium` handle (no worker, no
/// queue) — isolating the execution component. Under
/// `synchronous=FULL` every append's COMMIT is an fsync of the
/// WAL, so this distribution IS the commit/fsync distribution the
/// audit asked for; the delta against §1 is the queue overhead.
#[test]
fn f07_direct_provider_baseline_commits() {
    let db = temp_db("direct");
    let ops = f07_ops(200);
    let mut medium = engine_persist::SqliteMedium::open(&db).unwrap();
    let shared = payload_object(b"f07-direct-payload");
    // The founding publication (before the measured loop).
    let founding =
        RevisionRecord::founding(engine_persist::record::AuthorityMode::Standalone, "f07-dir", 0)
            .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let mut parent = Some(founding.id());
    let mut samples = Samples { micros: Vec::with_capacity(ops) };
    let start = Instant::now();
    for tick in 0..ops as u64 {
        let op_start = Instant::now();
        parent = Some(append_one(&mut medium, &shared, parent, tick));
        samples.record(op_start);
    }
    let wall = start.elapsed().as_secs_f64();
    samples.report("direct-provider", wall);
    samples.assert_consistent();
    println!(
        "[F07] direct-provider retained: db={}B wal={}B ops={}",
        file_len(&db),
        file_len(&db.with_extension("db-wal")),
        ops
    );
    drop(medium);
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
}
