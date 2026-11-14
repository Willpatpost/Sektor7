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
//!
//! **The F-11 remediation (§6, the post-audit directive's
//! CI-reliability finding).** The head-of-line material is SPLIT
//! by question: §6a proves the queue-ordering CORRECTNESS
//! deterministically (the worker's own `ParkNext`/`park_entered`
//! synchronization barrier — its wait BOUNDED per the final
//! review's RMD-02: a missing park-entry signal is a named
//! failure at a generous deadline, never an indefinite hang —
//! plus the append-parent dependency as the order detector: FIFO
//! established through causal observations and the authority
//! state, never a wall-clock magnitude), while §6b retains the
//! head-of-line LATENCY as observational distributions (printed,
//! never asserted — no absolute or relative timing thresholds, so
//! no scheduling noise, filesystem variance, or unusually slow
//! sample can fail the CI).

use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use engine_persist::{
    AppendOutcome, CanonicalMedium, Hash, RevisionRecord, StoredObject, SyncTier,
};
use engine_slice::store::{MediumSource, StoreLocation};
use engine_slice::worker::{DEFAULT_QUEUE_CAPACITY, IoWorker, WorkerFault, WorkerFaultControl};

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

/// (F-07 §6a, the F-11 remediation) The head-of-line queue-ordering
/// CORRECTNESS proof — deterministic, never a wall-clock magnitude.
/// The single-worker FIFO discipline is established through the
/// worker's own test-only synchronization seams (the `ParkNext`
/// barrier with `park_entered`/`release_park` — the established
/// deterministic replacements for timing-based "the worker holds
/// the job" assumptions) plus the append-parent dependency as the
/// order detector:
///
/// 1. A controlled slow operation is admitted FIRST: the append of
///    `r1`, armed with `ParkNext` — the worker dequeues the job and
///    parks BEFORE its execution (the `park_entered` barrier is the
///    real event, waited on directly, never a guessed duration).
/// 2. The second operation is admitted BEHIND it: the append of
///    `r2` (submitted while the worker is provably parked inside
///    `r1`'s job slot) whose EXPECTED PARENT is `r1`'s identity —
///    the authority state itself detects the completion order.
/// 3. The second cannot complete before the first's required
///    completion boundary: at the frozen point BOTH reply slots
///    are empty (`try_recv` → `Empty`, causally guaranteed — the
///    one worker thread is parked before `r1`'s execution, and a
///    reply slot can only be filled by that same thread), and
///    after the release `r2`'s `Accepted` outcome requires `r1`'s
///    durable commit (the CAS parent discipline refuses any
///    earlier execution).
/// 4. Both operations produce valid outcomes: both `Accepted`,
///    both records read back with the exact parent/epoch linkage.
/// 5. The authoritative revision state remains correct: the head
///    is `r2`, the height accounts for the founding load plus both
///    appends, and the full-chain validation passes.
#[test]
fn f07_head_of_line_queue_ordering_is_deterministic() {
    let db = temp_db("hol-order");
    // A small FIXED load: this is an ordering proof, not a scale
    // measurement (the F07_OPS deep runs exercise §1–§5 and §6b).
    let load = 8usize;
    let worker = IoWorker::spawn_with_faults(
        &StoreLocation::Database(db.to_path_buf()),
        DEFAULT_QUEUE_CAPACITY,
        WorkerFaultControl::new(),
    )
    .unwrap();
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let shared = payload_object(b"f07-hol-order-payload");
    let founding = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "f07-hol-o",
        0,
    )
    .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let mut parent = founding.id();
    for tick in 0..load as u64 {
        parent = append_one(&mut medium, &shared, Some(parent), tick);
    }
    // (1) The controlled slow operation, admitted FIRST: the append
    // of r1. The founding is epoch 0 and each regular append
    // advances the epoch by one, so after the load of 8 the tail
    // carries epoch 8 — r1's own epoch is 9.
    worker.fault_control().set(WorkerFault::ParkNext);
    let r1 =
        RevisionRecord::regular(parent, load as u64, 101).with_content_refs(vec![shared.hash()]);
    let r1_id = r1.id();
    let probe_slow = worker
        .submit_probe(engine_slice::worker::MediumOp::Append {
            record: r1,
            objects: vec![shared.clone()],
            expected_parent: Some(parent),
        })
        .unwrap();
    // The park-entry barrier: the worker HOLDS the slow job
    // (dequeued, pre-execution) — the deterministic event, never a
    // stall duration. The wait is BOUNDED (the RMD-02 corrective):
    // a worker failure or a missing park-entry signal converts to
    // a NAMED failure at the deadline, never an indefinite hang —
    // the bound is a failure diagnostic, NOT a timing assertion
    // (the synchronization remains the flag itself; the barrier is
    // never replaced by a guessed sleep, and the ordering proof
    // below is unchanged).
    let park_deadline = Instant::now() + Duration::from_secs(60);
    while !worker.fault_control().park_entered() {
        if Instant::now() > park_deadline {
            panic!(
                "the park-entry barrier never fired within 60s: the worker \
                 did not take-and-park the slow-op job (the ParkNext \
                 admission boundary — a worker death or a missing \
                 park-entry signal, never a slow machine)"
            );
        }
        std::hint::spin_loop();
    }
    // (2) The second operation, admitted BEHIND it: the append of
    // r2 whose expected parent IS r1 — the completion order is
    // detected through the authority state (r2 can only be
    // Accepted after r1's durable commit).
    let r2 =
        RevisionRecord::regular(r1_id, load as u64 + 1, 102).with_content_refs(vec![shared.hash()]);
    let r2_id = r2.id();
    let probe_behind = worker
        .submit_probe(engine_slice::worker::MediumOp::Append {
            record: r2,
            objects: vec![shared.clone()],
            expected_parent: Some(r1_id),
        })
        .unwrap();
    // (3) The frozen point: NEITHER reply exists — a causal
    // impossibility turned into a direct observation (the one
    // worker thread is parked inside r1's job slot, before r1's
    // execution; r2's reply slot can only be filled by that same
    // thread, so `Empty` here is deterministic, not a timing race).
    assert!(matches!(probe_slow.try_recv(), Err(TryRecvError::Empty)));
    assert!(matches!(probe_behind.try_recv(), Err(TryRecvError::Empty)));
    // (4) The release: the controlled slow op proceeds to its
    // required completion boundary (the durable commit, then the
    // reply deposit), and only then does r2 execute behind it.
    worker.fault_control().release_park();
    worker.fault_control().clear();
    match probe_slow.recv().unwrap() {
        Ok(engine_slice::worker::WorkerReply::Append(AppendOutcome::Accepted { id, .. })) => {
            assert_eq!(id, r1_id);
        }
        other => panic!("the slow-op append must be Accepted: {other:?}"),
    }
    match probe_behind.recv().unwrap() {
        Ok(engine_slice::worker::WorkerReply::Append(AppendOutcome::Accepted { id, .. })) => {
            assert_eq!(id, r2_id);
        }
        other => panic!("the behind append must be Accepted: {other:?}"),
    }
    // (5) The authoritative revision state: the head is r2, the
    // height accounts for the founding load plus both appends, and
    // the record reads back with the exact parent/epoch linkage.
    assert_eq!(medium.head().unwrap(), Some(r2_id));
    let readback = medium.read_record(&r2_id).unwrap();
    assert_eq!(readback.parent, Some(r1_id));
    assert_eq!(readback.epoch, load as u64 + 2);
    let summary = medium.validate().unwrap();
    assert_eq!(summary.height, load as u64 + 2);
    drop(medium);
    worker.join().unwrap();
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
}

/// (F-07 §6b, the F-11 remediation) The head-of-line LATENCY —
/// observational distributions only. The slow in-flight operation
/// (the full-chain validation, submitted fire-and-forget through the
/// worker's test-only probe) and one append timed behind it, over a
/// small fixed pair count: each pair prints its baseline, its
/// behind-the-slow-op latency, and the amplification factor; the
/// distributions are reported, never asserted (no absolute or
/// relative timing thresholds — the queue-ordering CORRECTNESS is
/// §6a's deterministic proof, so a scheduling-noise reversal here is
/// an honest measurement, never a CI failure).
#[test]
fn f07_head_of_line_block_behind_a_slow_operation() {
    let db = temp_db("hol");
    let ops = f07_ops(200);
    let pairs = 5usize;
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
    let mut epoch = ops as u64;
    let mut baselines = Samples {
        micros: Vec::with_capacity(pairs),
    };
    let mut behinds = Samples {
        micros: Vec::with_capacity(pairs),
    };
    for pair in 0..pairs {
        // The baseline: one more append's latency unencumbered.
        let baseline_start = Instant::now();
        parent = Some(append_one(&mut medium, &shared, parent, epoch));
        baselines.record(baseline_start);
        epoch += 1;
        // The slow in-flight operation: the full-chain validation,
        // submitted through the worker's probe (fire-and-forget).
        let probe = worker
            .submit_probe(engine_slice::worker::MediumOp::Validate)
            .unwrap();
        // The append timed behind it: submitted after the
        // validation is admitted, so its latency includes whatever
        // remains of the in-flight validation's execution — the
        // single-worker head-of-line effect the audit named.
        let behind_start = Instant::now();
        parent = Some(append_one(&mut medium, &shared, parent, epoch));
        behinds.record(behind_start);
        epoch += 1;
        // The probe's reply is the correctness fact (the validation
        // observed the definite chain state behind the pair's
        // baseline append); its latency contribution is already
        // inside the behind sample.
        match probe.recv().unwrap() {
            Ok(engine_slice::worker::WorkerReply::Validate(summary)) => {
                assert_eq!(summary.height, epoch - 1);
            }
            Ok(other) => panic!("the probe returned {other:?}"),
            Err(error) => panic!("the probe validation failed: {error:?}"),
        }
        println!(
            "[F07] head-of-line pair {pair}: baseline={}us behind={}us",
            baselines.micros[baselines.count() - 1],
            behinds.micros[behinds.count() - 1],
        );
    }
    // The pair distributions, reported observationally (no
    // throughput figure — the pair loop is a latency measurement,
    // not a rate one).
    println!(
        "[F07] head-of-line pairs: baseline mean={}us p50={}us p95={}us max={}us | behind mean={}us p50={}us p95={}us max={}us (ops={}, pairs={})",
        baselines.mean(),
        baselines.p50(),
        baselines.p95(),
        baselines.max(),
        behinds.mean(),
        behinds.p50(),
        behinds.p95(),
        behinds.max(),
        ops,
        pairs
    );
    // Correctness-only (never a performance gate): the samples are
    // populated and internally ordered; the final chain accounts
    // for the load plus every pair's two appends.
    baselines.assert_consistent();
    behinds.assert_consistent();
    assert_eq!(baselines.count(), pairs);
    assert_eq!(behinds.count(), pairs);
    assert_eq!(medium.head().unwrap(), parent);
    let summary = medium.validate().unwrap();
    assert_eq!(summary.height, epoch);
    drop(medium);
    worker.join().unwrap();
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(db.with_extension("db-wal"));
    let _ = std::fs::remove_file(db.with_extension("db-shm"));
}
