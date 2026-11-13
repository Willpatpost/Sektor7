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
    let founding = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "f07-seq",
        0,
    )
    .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let mut parent = Some(founding.id());
    let mut samples = Samples {
        micros: Vec::with_capacity(ops),
    };
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
    let founding = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "f07-dir",
        0,
    )
    .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let mut parent = Some(founding.id());
    let mut samples = Samples {
        micros: Vec::with_capacity(ops),
    };
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

/// (F-07 §3) The controlled contention: P producers, each with its
/// own queue proxy over the ONE worker, each appending with the
/// CAS retry discipline (a `RejectedStaleParent` rederives the
/// head — the realistic contended publication pattern). The
/// aggregate throughput stays bounded by the single serialized
/// worker (by design); the per-op latency grows with the queue
/// depth, and the retry count is the CAS-collision evidence.
#[test]
fn f07_controlled_contention_over_one_queue() {
    let db = temp_db("contend");
    let producers = 8usize;
    let per_producer = f07_ops(25);
    let worker = spawn_worker(&db);
    let source = worker.source().unwrap();
    let shared = payload_object(b"f07-contention-payload");
    // The founding publication (before the measured producers).
    let mut founding_proxy = source.clone().open_medium().unwrap();
    let founding = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "f07-ct",
        0,
    )
    .with_content_refs(vec![shared.hash()]);
    founding_proxy
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    drop(founding_proxy);
    let start = Instant::now();
    let handles: Vec<_> = (0..producers)
        .map(|_producer| {
            let source = source.clone();
            let shared = shared.clone();
            std::thread::spawn(move || {
                let mut medium = source.open_medium().unwrap();
                let mut samples = Samples {
                    micros: Vec::with_capacity(per_producer),
                };
                let mut retries = 0usize;
                for tick in 0..per_producer as u64 {
                    let op_start = Instant::now();
                    loop {
                        let head = medium.head().unwrap().unwrap();
                        let epoch = medium.read_record(&head).unwrap().epoch;
                        let record = RevisionRecord::regular(head, epoch, tick)
                            .with_content_refs(vec![shared.hash()]);
                        match medium.append(&record, std::slice::from_ref(&shared), Some(head)) {
                            Ok(AppendOutcome::Accepted { .. }) => break,
                            Ok(AppendOutcome::RejectedStaleParent { .. }) => {
                                retries += 1;
                                continue;
                            }
                            Ok(other) => {
                                panic!("the contended append must Accepted-or-Stale: {other:?}")
                            }
                            Err(error) => panic!("the contended append failed: {error:?}"),
                        }
                    }
                    samples.record(op_start);
                }
                (samples, retries)
            })
        })
        .collect();
    let mut all = Samples {
        micros: Vec::with_capacity(producers * per_producer),
    };
    let mut total_retries = 0usize;
    for handle in handles {
        let (samples, retries) = handle.join().unwrap();
        total_retries += retries;
        all.micros.extend(samples.micros);
    }
    let wall = start.elapsed().as_secs_f64();
    all.report("contention-8-producers", wall);
    all.assert_consistent();
    println!(
        "[F07] contention-8-producers: retries={} ops={} aggregate={:.0}ops/s",
        total_retries,
        all.count(),
        all.count() as f64 / wall
    );
    // The correctness check: the chain height equals the founding
    // plus every accepted append (the CAS discipline never loses
    // an accepted publication).
    let mut verify_proxy = source.open_medium().unwrap();
    let summary = verify_proxy.validate().unwrap();
    // ChainSummary.height carries the HEAD EPOCH (the sqlite
    // construction: `height: head_epoch` — the founding is epoch
    // 0, so N accepted appends surface as height N; the doc
    // comment's "height - 1" wording is the recorded mismatch).
    assert_eq!(
        summary.height as usize,
        producers * per_producer,
        "the chain height accounts for every accepted contended append"
    );
    drop(verify_proxy);
    drop(source);
    worker.join().unwrap();
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
}

/// (F-07 §4) The WAL growth and the checkpoint pause: the wal
/// file's retained size mid-run and after the load, then the
/// explicit TRUNCATE checkpoint's own latency through the queue
/// (the ratified lifecycle boundary) and the post-checkpoint wal
/// size.
#[test]
fn f07_wal_growth_and_checkpoint_pause() {
    let db = temp_db("wal");
    let ops = f07_ops(200);
    let worker = spawn_worker(&db);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let shared = payload_object(b"f07-wal-payload");
    let founding = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "f07-wal",
        0,
    )
    .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let mut parent = Some(founding.id());
    let mut samples = Samples {
        micros: Vec::with_capacity(ops),
    };
    let start = Instant::now();
    for tick in 0..ops as u64 {
        let op_start = Instant::now();
        parent = Some(append_one(&mut medium, &shared, parent, tick));
        samples.record(op_start);
        if tick % 64 == 0 {
            println!(
                "[F07] wal-growth sample at op {}: db={}B wal={}B",
                tick,
                file_len(&db),
                file_len(&db.with_extension("db-wal")),
            );
        }
    }
    let wall = start.elapsed().as_secs_f64();
    samples.report("wal-growth-load", wall);
    samples.assert_consistent();
    let wal_before = file_len(&db.with_extension("db-wal"));
    let checkpoint_start = Instant::now();
    medium.checkpoint().unwrap();
    let checkpoint_micros = checkpoint_start.elapsed().as_micros();
    let wal_after = file_len(&db.with_extension("db-wal"));
    println!(
        "[F07] checkpoint-truncate: latency={}us wal_before={}B wal_after={}B (ops={})",
        checkpoint_micros, wal_before, wal_after, ops
    );
    // The TRUNCATE checkpoint resets the wal (the ratified
    // lifecycle boundary; a correctness assert, not a timing one).
    assert_eq!(wal_after, 0, "the TRUNCATE checkpoint resets the wal");
    drop(medium);
    worker.join().unwrap();
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
}

/// (F-07 §5) The cold-reopen recovery: after the load and a clean
/// worker shutdown, a FRESH worker over the same database — the
/// reopen, the first head read, and the full authoritative-chain
/// validation (the cold walk over the grown chain) are the
/// recovery latency the audit asked for.
#[test]
fn f07_cold_reopen_recovery_latency() {
    let db = temp_db("recover");
    let ops = f07_ops(200);
    let worker = spawn_worker(&db);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let shared = payload_object(b"f07-recovery-payload");
    let founding = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "f07-rec",
        0,
    )
    .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let mut parent = Some(founding.id());
    for tick in 0..ops as u64 {
        parent = Some(append_one(&mut medium, &shared, parent, tick));
    }
    let expected_epoch = ops as u64;
    drop(medium);
    worker.join().unwrap();
    // The cold reopen: a FRESH worker over the same store.
    let reopen_start = Instant::now();
    let worker2 = spawn_worker(&db);
    let reopen_micros = reopen_start.elapsed().as_micros();
    let mut fresh = worker2.source().unwrap().open_medium().unwrap();
    let head_start = Instant::now();
    let head = fresh.head().unwrap().unwrap();
    let head_micros = head_start.elapsed().as_micros();
    assert_eq!(head, parent.expect("the load appended records"));
    let validate_start = Instant::now();
    let summary = fresh.validate().unwrap();
    let validate_micros = validate_start.elapsed().as_micros();
    println!(
        "[F07] cold-recovery: reopen={}us first_head={}us full_validate={}us (ops={}, height={})",
        reopen_micros, head_micros, validate_micros, ops, summary.height
    );
    assert_eq!(summary.height, expected_epoch);
    let _ = validate_micros;
    drop(fresh);
    worker2.join().unwrap();
    println!(
        "[F07] cold-recovery retained: db={}B wal={}B",
        file_len(&db),
        file_len(&db.with_extension("db-wal")),
    );
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
}

/// (F-07 §6) The head-of-line demonstration: a slow operation
/// (the full-chain validation) submitted without waiting (the
/// worker's test-only probe), then one append timed behind it —
/// the append's latency includes the in-flight validation's
/// remaining execution, exactly the single-worker head-of-line
/// effect the audit named as the risk to measure.
#[test]
fn f07_head_of_line_block_behind_a_slow_operation() {
    let db = temp_db("hol");
    let ops = f07_ops(200);
    let worker = spawn_worker(&db);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let shared = payload_object(b"f07-hol-payload");
    let founding = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "f07-hol",
        0,
    )
    .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let mut parent = Some(founding.id());
    for tick in 0..ops as u64 {
        parent = Some(append_one(&mut medium, &shared, parent, tick));
    }
    // The baseline: one more append's latency unencumbered.
    let baseline_start = Instant::now();
    parent = Some(append_one(&mut medium, &shared, parent, ops as u64));
    let baseline_micros = baseline_start.elapsed().as_micros();
    // The slow in-flight operation: the full-chain validation,
    // submitted through the worker's probe (fire-and-forget).
    let probe = worker
        .submit_probe(engine_slice::worker::MediumOp::Validate)
        .unwrap();
    let behind_start = Instant::now();
    append_one(&mut medium, &shared, parent, ops as u64 + 1);
    let behind_micros = behind_start.elapsed().as_micros();
    let validation = match probe.recv().unwrap() {
        Ok(engine_slice::worker::WorkerReply::Validate(summary)) => summary.height,
        Ok(other) => panic!("the probe returned {other:?}"),
        Err(error) => panic!("the probe validation failed: {error:?}"),
    };
    println!(
        "[F07] head-of-line: baseline_append={}us append_behind_validate={}us (ops={}, epoch={})",
        baseline_micros, behind_micros, ops, validation
    );
    // The append behind the in-flight validation is slower — the
    // magnitude is the measurement (asserted only as ordering,
    // never a fixed threshold).
    assert!(
        behind_micros > baseline_micros,
        "the queued append waits for the in-flight validation: {behind_micros}us vs {baseline_micros}us"
    );
    drop(medium);
    worker.join().unwrap();
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
}
