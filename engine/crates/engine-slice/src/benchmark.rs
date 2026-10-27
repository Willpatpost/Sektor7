//! Stage 5 Batch 5: the persistence performance benchmark (the GO's
//! fourth item) — measured on the REAL workload (the 1000-NPC Lod
//! scenario at the accrual peak), reported as distributions with the
//! workload parameters, and interpreted against the 60/144/240 FPS
//! frame budgets. **Pod results are NOT authoritative physical-rig
//! results** — the Windows rig's evidence remains the owner's (the
//! physical-rig instructions in the Batch 5 record).
//!
//! The measured operations (each a distribution over N iterations):
//! - **Snapshot serialization** (`save_session` — the spine save with
//!   the world section plus the director state, no I/O).
//! - **Snapshot publication** (`publish_snapshot` — the serialization
//!   plus the B2 nine-step append with its fsyncs; the chain grows one
//!   revision per publication).
//! - **T1 journal transaction** (the six-phase deactivation's durable
//!   composite — the envelope staging, the plan blob, the journal
//!   append with its fsync, the post-fsync verify, and the in-memory
//!   install — measured on fresh campaigns so every sample is a first
//!   transaction on a fresh journal).
//! - **Canonical confirmation** (`commit_canonical` — the batch
//!   submission against the verified head and the read-back
//!   validation).
//! - **Session recovery** (`SessionCrossing::recover` — the snapshot
//!   restore, the journal scan, the plan re-install, the anchor
//!   reconcile, and the re-opened crossing).
//! - **Chain cold-open validation** (the B2 founding→HEAD walk at
//!   growing chain lengths).
//! - **Storage growth** (the journal bytes per first transaction, the
//!   store bytes per publication, and the GROWTH SWEEP across
//!   increasing campaign durations — the lifetime-growing execution
//!   ledger/indexes/snapshot vs the bounded active obligations).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine_core::{HistoryKind, PresenceSample, Spine};
use engine_persist::{
    AuthorityMode, CanonicalMedium, CommitOutcome, LocalFileMedium, RevisionRecord, SqliteMedium,
    T1Journal,
};

use crate::crossing::{SessionCrossing, restore_session, save_session};
use crate::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use crate::orchestration::{OrchestratedSession, SubmitRefusal};
use crate::persist_demo::{DemoOptions, run_persist_demo_headless};
use crate::region::{RegionConfig, RegionDirector};
use crate::soak_evidence::{EvidenceCollector, EvidenceConfig, EvidenceRecord, TickSample};
use crate::store::{SessionStore, StoreLocation};
use crate::worker::WorkerFault;

/// One measured operation's distribution, printed as one line.
fn report(name: &str, samples: &mut [Duration]) {
    samples.sort_unstable();
    let n = samples.len();
    let min = samples[0].as_secs_f64() * 1e3;
    let median = samples[n / 2].as_secs_f64() * 1e3;
    let p95_index = (((n as f64) * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(n - 1);
    let p95 = samples[p95_index].as_secs_f64() * 1e3;
    let p99_index = (((n as f64) * 0.99).ceil() as usize)
        .saturating_sub(1)
        .min(n - 1);
    let p99 = samples[p99_index].as_secs_f64() * 1e3;
    let max = samples[n - 1].as_secs_f64() * 1e3;
    println!(
        "  {name}: min {min:.3} / median {median:.3} / p95 {p95:.3} / p99 {p99:.3} / max {max:.3} ms (n={n})"
    );
}

/// The recursive directory size in bytes (the storage-growth
/// measurement).
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// The real scenario pair (the crossing suite's fixture, local to this
/// module): the Lod-mode spine plus the director, driven through the
/// driver's own per-tick sequence.
fn scenario_pair(ticks: u64, presence: &PresenceSample) -> (Spine<NpcWorld>, RegionDirector) {
    let config = NpcConfig {
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
    install_initial_schedules(&mut spine, &config);
    let mut director = RegionDirector::new(RegionConfig::default()).unwrap();
    for _ in 0..ticks {
        director.apply(&mut spine, std::slice::from_ref(presence));
        spine.run_tick();
    }
    (spine, director)
}

/// A unique campaign directory under the system temp dir.
fn campaign_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "s7-b5-benchmark-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// The pre-founded campaign store for the ORCHESTRATED measurements
/// (the worker discipline: the store founded BEFORE the worker spawns
/// — the same pre-founding every orchestrated test uses).
fn found_benchmark_store(dir: &Path, database: bool) -> StoreLocation {
    let location = if database {
        StoreLocation::Database(dir.join("campaign.db"))
    } else {
        StoreLocation::Directory(dir.join("store"))
    };
    let mut store = SessionStore::open(&location).unwrap();
    if store.head().unwrap().is_none() {
        store
            .append(
                &RevisionRecord::founding(AuthorityMode::Standalone, "b5-bench", 0),
                &[],
                None,
            )
            .unwrap();
    }
    location
}

/// One retention-measurement campaign (the PP-B3 benchmark's local
/// fixture): the publications, the two checkpoints, and the audited
/// reclamation of the oldest range — the lean form (no confirmation
/// cycles: the storage effect is the measurement, not the lane
/// machinery). Returns (the store bytes before, the store bytes
/// after, the objects removed).
fn retention_campaign(
    root: &Path,
    label: &str,
    ticks: u64,
    publications_per_range: usize,
    generations: u64,
) -> (u64, u64, u64) {
    let near = PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    };
    let dir = root.join(label);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut spine, mut director) = scenario_pair(ticks, &near);
    let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.join("store"), journal, "benchmark-retention").unwrap();
    for _ in 0..publications_per_range {
        for _ in 0..20 {
            director.apply(&mut spine, std::slice::from_ref(&near));
            spine.run_tick();
        }
        crossing.publish_snapshot(&spine, &director).unwrap();
    }
    crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .unwrap();
    for _ in 0..publications_per_range {
        for _ in 0..20 {
            director.apply(&mut spine, std::slice::from_ref(&near));
            spine.run_tick();
        }
        crossing.publish_snapshot(&spine, &director).unwrap();
    }
    crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .unwrap();
    let before = dir_size(&dir.join("store"));
    let reclamation = crossing
        .reclaim_checkpoint(&dir.join("journal.t1"), generations)
        .unwrap();
    let after = dir_size(&dir.join("store"));
    drop(crossing);
    let _ = std::fs::remove_dir_all(&dir);
    (before, after, reclamation.removed_objects)
}

/// Runs the persistence benchmark and prints the report.
pub fn run() {
    println!(
        "Sektor7 Stage 5 Batch 5 — persistence benchmark (POD results — NOT authoritative physical-rig evidence)"
    );
    println!(
        "workload: 1000 NPCs / 8 regions / the Lod scenario at 150 ticks (the accrual peak) / budget 200 near-reserve 60"
    );
    println!("campaign root: {}", std::env::temp_dir().display());
    let near = PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    };
    let root = campaign_dir("root");

    // 1. The snapshot serialization (no I/O) — the pure codec cost.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut serialization = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let bytes = save_session(&spine, &director).unwrap();
        serialization.push(start.elapsed());
        let _ = bytes.len();
    }
    let save_len = save_session(&spine, &director).unwrap().len();
    report(
        "snapshot serialization (save_session, no I/O)",
        &mut serialization,
    );

    // 2. The snapshot publication — the serialization plus the B2
    //    nine-step append with its fsyncs; the chain grows one
    //    revision per publication, and the cold-open walk is measured
    //    at growing lengths.
    let store = root.join("store");
    let journal_path = root.join("journal.t1");
    let journal = T1Journal::open(&journal_path).unwrap();
    let mut crossing = SessionCrossing::open(&store, journal, "benchmark").unwrap();
    let mut publication = Vec::new();
    let mut chain_walks: Vec<(usize, Duration, u64)> = Vec::new();
    let mut store_bytes_before = dir_size(&store);
    let mut publication_growth = Vec::new();
    for i in 0..20 {
        // Each publication a DISTINCT session state — the idempotency
        // key (the snapshot's content hash) is chain-unique, so the
        // session advances one tick per publication (the publications
        // sample ticks 151-170, the accrual continuing).
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
        let start = Instant::now();
        crossing.publish_snapshot(&spine, &director).unwrap();
        publication.push(start.elapsed());
        let store_bytes_after = dir_size(&store);
        publication_growth.push(store_bytes_after - store_bytes_before);
        store_bytes_before = store_bytes_after;
        let revisions = i + 2; // the founding record plus the publications.
        if [6, 11, 21].contains(&revisions) {
            let start = Instant::now();
            let _medium = LocalFileMedium::open(store.clone()).unwrap();
            chain_walks.push((revisions, start.elapsed(), store_bytes_after));
        }
    }
    report(
        "snapshot publication (the B2 append incl. fsync)",
        &mut publication,
    );
    for (revisions, walk, bytes) in &chain_walks {
        println!(
            "  chain cold-open validation at {revisions} revisions: {:.3} ms (store {bytes} B)",
            walk.as_secs_f64() * 1e3
        );
    }
    let growth: u64 = publication_growth.iter().sum();
    println!(
        "  store growth: ~{} B per snapshot publication (total {} B over 20)",
        growth / 20,
        growth
    );
    drop(crossing);

    // 3+4. The T1 journal transaction and the canonical confirmation —
    //     fresh campaigns so every sample is a first transaction on a
    //     fresh journal (the six-phase deactivation's durable
    //     composite, then the batch submission).
    let mut transaction = Vec::new();
    let mut confirmation = Vec::new();
    let mut journal_first_sizes = Vec::new();
    for campaign in 0..10 {
        let (mut spine, mut director) = scenario_pair(150, &near);
        let dir = campaign_dir(&format!("txn-{campaign}"));
        let store = dir.join("store");
        let journal_path = dir.join("journal.t1");
        let journal = T1Journal::open(&journal_path).unwrap();
        let mut crossing = SessionCrossing::open(&store, journal, "benchmark").unwrap();
        let now = spine.clock();
        let start = Instant::now();
        crossing
            .deactivate(&mut spine, &mut director, 4, now)
            .unwrap()
            .expect("the scenario captured consequences");
        transaction.push(start.elapsed());
        let start = Instant::now();
        let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
            panic!("the confirmation confirms");
        };
        confirmation.push(start.elapsed());
        drop(crossing);
        journal_first_sizes.push(std::fs::metadata(&journal_path).unwrap().len());
        let _ = std::fs::remove_dir_all(&dir);
    }
    report(
        "T1 journal transaction (the durable deactivation composite)",
        &mut transaction,
    );
    report(
        "canonical confirmation (commit_canonical)",
        &mut confirmation,
    );
    let journal_min = journal_first_sizes.iter().min().unwrap();
    let journal_max = journal_first_sizes.iter().max().unwrap();

    // 5. The session recovery — the snapshot + one durable transaction
    //    on disk, recovered repeatedly (the recovery is a pure
    //    function of the durable state).
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = campaign_dir("recovery");
    let recovery_store = dir.join("store");
    let recovery_journal = dir.join("journal.t1");
    let journal = T1Journal::open(&recovery_journal).unwrap();
    let mut crossing = SessionCrossing::open(&recovery_store, journal, "benchmark").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the scenario captured consequences");
    drop(crossing);
    let mut recovery = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let (_spine, _director, _crossing) =
            SessionCrossing::recover(&recovery_store, &recovery_journal, "benchmark").unwrap();
        recovery.push(start.elapsed());
    }
    report("session recovery (recover)", &mut recovery);

    // 5b. THE SQLITE PROVIDER PATH (PP-B1's review corrective — the
    //     release-performance evidence): the same measured operations
    //     over the Database provider (the SQLite canonical medium,
    //     D-PP-1), printed alongside the file-medium numbers above as
    //     the reference comparison. The workload is identical.
    println!("SQLite provider path (PP-B1 — the Database provider, the same workload):");
    let (mut spine, mut director) = scenario_pair(150, &near);
    let sqlite_root = campaign_dir("sqlite");
    let sqlite_db = sqlite_root.join("store.db");
    let journal = T1Journal::open(sqlite_root.join("journal.t1")).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(sqlite_db.clone()),
        journal,
        "benchmark-sqlite",
    )
    .unwrap();
    let mut sqlite_publication = Vec::new();
    let mut db_bytes_before = std::fs::metadata(&sqlite_db).map(|m| m.len()).unwrap_or(0);
    let mut sqlite_publication_growth = Vec::new();
    for _ in 0..20 {
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
        let start = Instant::now();
        crossing.publish_snapshot(&spine, &director).unwrap();
        sqlite_publication.push(start.elapsed());
        let db_bytes_after = std::fs::metadata(&sqlite_db).map(|m| m.len()).unwrap_or(0);
        sqlite_publication_growth.push(db_bytes_after - db_bytes_before);
        db_bytes_before = db_bytes_after;
    }
    report(
        "snapshot publication (the SQLite append incl. commit)",
        &mut sqlite_publication,
    );
    let growth: u64 = sqlite_publication_growth.iter().sum();
    println!(
        "  db growth: ~{} B per snapshot publication (total {} B over 20)",
        growth / 20,
        growth
    );
    drop(crossing);
    // The chain cold-open validation (the session open's founding→HEAD
    // walk) over the SQLite database.
    let start = Instant::now();
    let _medium = SqliteMedium::open(&sqlite_db).unwrap();
    println!(
        "  chain cold-open validation at 21 revisions: {:.3} ms (db {db_bytes_before} B)",
        start.elapsed().as_secs_f64() * 1e3
    );
    // The canonical confirmation over SQLite (fresh campaigns, the
    // same discipline as the file-medium section).
    let mut sqlite_confirmation = Vec::new();
    for campaign in 0..10 {
        let (mut spine, mut director) = scenario_pair(150, &near);
        let dir = campaign_dir(&format!("sqlite-txn-{campaign}"));
        let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
        let mut crossing = SessionCrossing::open_at(
            &StoreLocation::Database(dir.join("store.db")),
            journal,
            "benchmark-sqlite",
        )
        .unwrap();
        let now = spine.clock();
        crossing
            .deactivate(&mut spine, &mut director, 4, now)
            .unwrap()
            .expect("the scenario captured consequences");
        let start = Instant::now();
        let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
            panic!("the confirmation confirms");
        };
        sqlite_confirmation.push(start.elapsed());
        drop(crossing);
        let _ = std::fs::remove_dir_all(&dir);
    }
    report(
        "canonical confirmation (commit_canonical, SQLite)",
        &mut sqlite_confirmation,
    );
    // The session recovery over SQLite.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = campaign_dir("sqlite-recovery");
    let recovery_db = dir.join("store.db");
    let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(recovery_db.clone()),
        journal,
        "benchmark-sqlite",
    )
    .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the scenario captured consequences");
    drop(crossing);
    let mut sqlite_recovery = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let (_spine, _director, _crossing) = SessionCrossing::recover_at(
            &StoreLocation::Database(recovery_db.clone()),
            &dir.join("journal.t1"),
            "benchmark-sqlite",
        )
        .unwrap();
        sqlite_recovery.push(start.elapsed());
    }
    report(
        "session recovery (recover_at, SQLite)",
        &mut sqlite_recovery,
    );
    let _ = std::fs::remove_dir_all(&sqlite_root);
    let _ = std::fs::remove_dir_all(&dir);

    // 6. The storage-growth sweep (the review corrective): the spine's
    //    execution LEDGER (one record per execution, serialized in
    //    full fidelity), the KNOWN/EXECUTED indexes (one entry per
    //    scheduled/executed identity — in-memory, rebuilt from
    //    next_seq + unfinished on load), and therefore the SERIALIZED
    //    SNAPSHOT all grow with LIFETIME execution history — the
    //    150-tick baseline cannot establish long-campaign boundedness.
    //    Measured across increasing campaign durations: the executed
    //    count (the ledger's record count), the scheduled count (the
    //    known index's size), the serialized spine save, the session
    //    snapshot, and the codec time — deriving the per-execution
    //    growth rate and the per-record codec cost.
    println!("storage-growth sweep (the LIFETIME history vs the bounded active state):");
    let mut sweep_first: Option<(u64, usize, u64)> = None; // (executed, spine_len, ticks)
    let mut sweep_last: (u64, usize, u64) = (0, 0, 0);
    for ticks in [150u64, 600, 1200, 2400, 4800] {
        let (spine, director) = scenario_pair(ticks, &near);
        let stats = spine.stats();
        let spine_len = spine.save_bytes().len();
        let session_len = save_session(&spine, &director).unwrap().len();
        let start = Instant::now();
        let _ = save_session(&spine, &director).unwrap();
        let ser = start.elapsed();
        println!(
            "  {ticks} ticks: executed {} / scheduled {} (the known index) / the spine save {spine_len} B / the session snapshot {session_len} B / the codec {:.3} ms",
            stats.executed,
            stats.scheduled,
            ser.as_secs_f64() * 1e3
        );
        if sweep_first.is_none() {
            sweep_first = Some((stats.executed, spine_len, ticks));
        }
        sweep_last = (stats.executed, spine_len, ticks);
    }
    if let Some((first_exec, first_len, _)) = sweep_first {
        let per_execution =
            (sweep_last.1 as f64 - first_len as f64) / (sweep_last.0 as f64 - first_exec as f64);
        println!(
            "  derived: ~{per_execution:.1} B per execution (the ledger's serialized share — the dominant lifetime growth); ~{:.0} executions per tick in this fixture",
            sweep_last.0 as f64 / sweep_last.2 as f64
        );
    }

    // 7. THE PP-B2 COMPACTION MEASUREMENTS (spine-history/2 + the
    //    consequence-lane extension): the compaction cost, the
    //    compacted serialization/deserialization, the publication and
    //    the recovery over compacted pairs, the window sensitivity,
    //    and the full-vs-compacted growth sweep — the bounded-history
    //    claim measured against the design's §7.3 projections (pod
    //    results, NOT authoritative physical-rig evidence; the
    //    Windows rig's release-mode evidence remains the owner's).
    println!(
        "PP-B2 compaction (spine-history/2 — pod results, NOT authoritative physical-rig evidence):"
    );
    // 7a. The compaction cost — fresh scenarios, each compacted once
    //     (the operation mutates the history, so every sample is a
    //     fresh 150-tick history).
    let mut compaction = Vec::new();
    for _ in 0..10 {
        let (mut spine, mut director) = scenario_pair(150, &near);
        let dir = campaign_dir("compact-cost");
        let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
        let mut crossing =
            SessionCrossing::open(&dir.join("store"), journal, "benchmark-compaction").unwrap();
        let start = Instant::now();
        crossing
            .compact_session(&mut spine, &mut director, 100)
            .unwrap();
        compaction.push(start.elapsed());
        drop(crossing);
        let _ = std::fs::remove_dir_all(&dir);
    }
    report(
        "session compaction (compact_session, the 150-tick history)",
        &mut compaction,
    );

    // 7b. The compacted codec at 150 ticks — the compacted form's
    //     serialization and deserialization, with the full form's
    //     deserialization beside it (the full serialization is
    //     section 1 above).
    let (mut spine, mut director) = scenario_pair(150, &near);
    let full_session_len = save_session(&spine, &director).unwrap().len();
    let dir = campaign_dir("compact-codec");
    let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.join("store"), journal, "benchmark-compaction").unwrap();
    crossing
        .compact_session(&mut spine, &mut director, 100)
        .unwrap();
    let mut compacted_serialization = Vec::new();
    let mut compacted_bytes = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        compacted_bytes = save_session(&spine, &director).unwrap();
        compacted_serialization.push(start.elapsed());
    }
    let compacted_session_len = compacted_bytes.len();
    let mut compacted_deserialization = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let _ = restore_session(&compacted_bytes).unwrap();
        compacted_deserialization.push(start.elapsed());
    }
    report(
        "compacted serialization (save_session, no I/O)",
        &mut compacted_serialization,
    );
    report(
        "compacted deserialization (restore_session)",
        &mut compacted_deserialization,
    );
    let (full_spine, full_director) = scenario_pair(150, &near);
    let full_bytes = save_session(&full_spine, &full_director).unwrap();
    let mut full_deserialization = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let _ = restore_session(&full_bytes).unwrap();
        full_deserialization.push(start.elapsed());
    }
    report(
        "full deserialization (restore_session)",
        &mut full_deserialization,
    );
    println!(
        "  the session snapshot at 150 ticks: full {full_session_len} B vs compacted {compacted_session_len} B (retain 100 records)"
    );

    // 7c. The publication and the recovery over the compacted form.
    //     The publication samples ride the codec campaign above; the
    //     recovery uses its OWN campaign in exactly the section-5
    //     shape (one publication plus one durable transaction — the
    //     same chain length), so the comparison against the full-path
    //     recovery isolates the compacted snapshot's effect.
    let mut compacted_publication = Vec::new();
    for _ in 0..10 {
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
        let start = Instant::now();
        crossing.publish_snapshot(&spine, &director).unwrap();
        compacted_publication.push(start.elapsed());
    }
    report(
        "compacted snapshot publication (the B2 append incl. fsync)",
        &mut compacted_publication,
    );
    drop(crossing);
    let _ = std::fs::remove_dir_all(&dir);
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = campaign_dir("compact-recovery");
    let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.join("store"), journal, "benchmark-compaction").unwrap();
    crossing
        .compact_session(&mut spine, &mut director, 100)
        .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the scenario captured consequences");
    drop(crossing);
    let mut compacted_recovery = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let (_spine, _director, _crossing) = SessionCrossing::recover(
            &dir.join("store"),
            &dir.join("journal.t1"),
            "benchmark-compaction",
        )
        .unwrap();
        compacted_recovery.push(start.elapsed());
    }
    report(
        "session recovery from the compacted pair (recover)",
        &mut compacted_recovery,
    );
    let _ = std::fs::remove_dir_all(&dir);

    // 7d. The full-vs-compacted growth sweep (one scenario per
    //     duration: the full session measured first, then the SAME
    //     history compacted) and the window sensitivity at the
    //     longest duration — the bounded steady state against the
    //     design's §7.3 projection.
    println!("compaction growth sweep (the full lifetime growth vs the compacted steady state):");
    for ticks in [150u64, 600, 1200, 2400, 4800] {
        let (mut sweep_spine, mut sweep_director) = scenario_pair(ticks, &near);
        let full_len = save_session(&sweep_spine, &sweep_director).unwrap().len();
        let cdir = campaign_dir("sweep-compact");
        let cjournal = T1Journal::open(cdir.join("journal.t1")).unwrap();
        let mut ccrossing =
            SessionCrossing::open(&cdir.join("store"), cjournal, "benchmark-compaction").unwrap();
        let start = Instant::now();
        let compact_report = ccrossing
            .compact_session(&mut sweep_spine, &mut sweep_director, 100)
            .unwrap();
        let compaction_ms = start.elapsed().as_secs_f64() * 1e3;
        let compacted_len = save_session(&sweep_spine, &sweep_director).unwrap().len();
        let summarized = match compact_report.history {
            HistoryKind::Compacted {
                summarized_records, ..
            } => summarized_records,
            HistoryKind::Full => 0,
        };
        println!(
            "  {ticks} ticks: full {full_len} B vs compacted {compacted_len} B (retain 100) / the compaction {compaction_ms:.3} ms / summarized {summarized} records"
        );
        drop(ccrossing);
        let _ = std::fs::remove_dir_all(&cdir);
    }
    println!(
        "window sensitivity (the 4800-tick history, the compacted snapshot by retained window):"
    );
    for retain in [0u64, 100, 1000, 5000] {
        let (mut sweep_spine, mut sweep_director) = scenario_pair(4800, &near);
        let cdir = campaign_dir("window");
        let cjournal = T1Journal::open(cdir.join("journal.t1")).unwrap();
        let mut ccrossing =
            SessionCrossing::open(&cdir.join("store"), cjournal, "benchmark-compaction").unwrap();
        let start = Instant::now();
        ccrossing
            .compact_session(&mut sweep_spine, &mut sweep_director, retain)
            .unwrap();
        let compaction_ms = start.elapsed().as_secs_f64() * 1e3;
        let compacted_len = save_session(&sweep_spine, &sweep_director).unwrap().len();
        println!("  retain {retain}: {compacted_len} B / the compaction {compaction_ms:.3} ms");
        drop(ccrossing);
        let _ = std::fs::remove_dir_all(&cdir);
    }
    println!(
        "  design §7.3 projection: the compacted steady state ≈250-400 KB (the active-state scale + the retained window) — compare the measured compacted sizes above"
    );

    // 8. PP-B3 — the canonical retention economics (the checkpoint +
    // the reclamation + the rotation): the storage before/after the
    // audited reclamation across the campaign sweep, the interval
    // sensitivity (the publication count per range), and the
    // operation costs.
    println!("8. PP-B3 retention (the checkpoint + the reclamation + the rotation)");
    // 8a. The storage sweep: the lean two-checkpoint campaign at
    // increasing durations; the audited reclamation of the oldest
    // range under the generations=1 window.
    for ticks in [150, 600, 1200, 2400] {
        let (before, after, removed) =
            retention_campaign(&root, &format!("sweep-{ticks}"), ticks, 2, 1);
        let reduction = 100.0 * (before - after) as f64 / before as f64;
        println!(
            "  {ticks} ticks: {before} B → {after} B after the reclamation (−{reduction:.1}%, {removed} objects removed)"
        );
    }
    // 8b. The interval sensitivity: the publications per range (2 vs
    // 8) — the reclaimed bytes scale with the range size while the
    // retained anchors and metadata stay flat.
    for publications in [2, 8] {
        let (before, after, removed) = retention_campaign(
            &root,
            &format!("interval-{publications}"),
            600,
            publications,
            1,
        );
        println!(
            "  {publications} publications per range: {before} B → {after} B ({removed} objects removed)"
        );
    }
    // 8c. The operation costs (fresh scenarios — the operations
    // mutate): the checkpoint publication, the journal rotation (over
    // one outstanding journal transaction), and the reclamation.
    let mut checkpoint_samples = Vec::new();
    let mut rotation_samples = Vec::new();
    let mut reclamation_samples = Vec::new();
    for index in 0..5 {
        let dir = root.join(format!("costs-{index}"));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut spine, mut director) = scenario_pair(600, &near);
        let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
        let mut crossing =
            SessionCrossing::open(&dir.join("store"), journal, "benchmark-costs").unwrap();
        crossing.publish_snapshot(&spine, &director).unwrap();
        for _ in 0..20 {
            director.apply(&mut spine, std::slice::from_ref(&near));
            spine.run_tick();
        }
        crossing.publish_snapshot(&spine, &director).unwrap();
        let start = Instant::now();
        crossing
            .publish_checkpoint(&mut spine, &mut director, 10)
            .unwrap();
        checkpoint_samples.push(start.elapsed());
        for _ in 0..2 {
            for _ in 0..20 {
                director.apply(&mut spine, std::slice::from_ref(&near));
                spine.run_tick();
            }
            crossing.publish_snapshot(&spine, &director).unwrap();
        }
        crossing
            .publish_checkpoint(&mut spine, &mut director, 10)
            .unwrap();
        // One outstanding journal transaction — the rotation's
        // survivor set.
        let now = spine.clock();
        crossing
            .deactivate(&mut spine, &mut director, 4, now)
            .unwrap();
        let start = Instant::now();
        crossing.rotate_journal().unwrap();
        rotation_samples.push(start.elapsed());
        let start = Instant::now();
        crossing
            .reclaim_checkpoint(&dir.join("journal.t1"), 1)
            .unwrap();
        reclamation_samples.push(start.elapsed());
        drop(crossing);
        let _ = std::fs::remove_dir_all(&dir);
    }
    report(
        "the checkpoint publication (compact + SK + MK)",
        &mut checkpoint_samples,
    );
    report(
        "the journal rotation (the survivor re-anchor + the flip)",
        &mut rotation_samples,
    );
    report(
        "the reclamation (the audit + the deletes)",
        &mut reclamation_samples,
    );
    println!(
        "  interpretation: docs/PP-B3-RETENTION-RECORD.md (the measured-vs-budget analysis; pod results, not physical-rig evidence)"
    );

    // ------------------------------------------------------------------
    // 9. PP-B4 ORCHESTRATION (the worker-backed authoritative session)
    //
    // The PP-B3 measurements that motivated PP-B4 are section 8 above
    // (the crossing-level maintenance compounds); this section
    // measures the WORKER-BACKED implementation under equivalent
    // workloads. HONESTY (the final-validation GO's rule): the
    // optional publication path is asynchronous for the AUTHORITY
    // THREAD (a bounded enqueue, a typed Busy at the boundary); T1
    // durability and protected maintenance retain their accepted
    // SYNCHRONOUS boundaries — never a claim that all persistence is
    // nonblocking. The 16.667 ms frame budget is the comparison
    // reference, never an invented guaranteed maximum. Pod
    // dev-profile evidence only — never the owner's physical rig.
    println!(
        "9. PP-B4 orchestration (the worker-backed session — pod results, not physical-rig evidence)"
    );
    {
        let far = PresenceSample {
            position: (100.0, 0.0),
            velocity: (0.0, 0.0),
        };
        let near_presence = [near];
        let far_presence = [far];

        // 9a — THE AUTHORITATIVE TICK (the idle worker; warmup
        // separated from measurement).
        let dir = campaign_dir("orchestration-tick");
        let location = found_benchmark_store(&dir, false);
        let (spine, director) = scenario_pair(150, &near);
        let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
        let mut session =
            OrchestratedSession::open(spine, director, &location, journal, "b5-bench").unwrap();
        for _ in 0..50 {
            session.tick(&near_presence).unwrap();
        }
        let mut tick_samples = Vec::new();
        for _ in 0..300 {
            let start = Instant::now();
            session.tick(&near_presence).unwrap();
            tick_samples.push(start.elapsed());
        }
        report("the authoritative tick (idle worker)", &mut tick_samples);

        // 9b — THE TICK DURING AN EXECUTING (PARKED) OPTIONAL
        // PUBLICATION: the worker holds the append in its hands; the
        // authority thread continues — the tick's T1 durability is
        // journal-side, never through the worker (the responsiveness
        // proof, deterministically parked through the entry barrier).
        session
            .fault_control()
            .set(WorkerFault::ParkExecutingAppend);
        let parked_handle = session
            .submit_publication(session.capture_snapshot().unwrap())
            .unwrap();
        while !session.fault_control().park_entered() {
            std::hint::spin_loop();
        }
        let mut parked_tick_samples = Vec::new();
        for _ in 0..100 {
            let start = Instant::now();
            session.tick(&near_presence).unwrap();
            parked_tick_samples.push(start.elapsed());
        }
        session.fault_control().release_park();
        let _ = session.wait_publication(parked_handle).unwrap();
        report(
            "the tick during an executing publication (parked worker)",
            &mut parked_tick_samples,
        );

        // 9c — THE SNAPSHOT CAPTURE (the serialization + the observed
        // head + the frontier scan — read-only, on the authority
        // thread).
        let mut capture_samples = Vec::new();
        for _ in 0..50 {
            let start = Instant::now();
            session.capture_snapshot().unwrap();
            capture_samples.push(start.elapsed());
        }
        report("the snapshot capture", &mut capture_samples);

        // 9d — THE OPTIONAL ADMISSION LATENCY + THE SATURATION
        // BOUNDARY: a state-advancing tick (the fresh content), every
        // capture BEFORE the first submission (the observe-head FIFO
        // discipline), submit until the typed Busy, then drain.
        session.tick(&near_presence).unwrap();
        let captures: Vec<_> = (0..70)
            .map(|_| session.capture_snapshot().unwrap())
            .collect();
        let mut admission_samples = Vec::new();
        let mut handles = Vec::new();
        let mut busy = 0usize;
        for capture in captures {
            let start = Instant::now();
            match session.submit_publication(capture) {
                Ok(handle) => {
                    admission_samples.push(start.elapsed());
                    handles.push(handle);
                }
                Err(SubmitRefusal::Busy) => busy += 1,
                Err(other) => panic!("the admission refusal: {other:?}"),
            }
        }
        for handle in handles {
            let _ = session.wait_publication(handle).unwrap();
        }
        println!(
            "  the saturation boundary: {} admitted / {busy} Busy (the bounded queue; the depth at the boundary is the admitted count)",
            admission_samples.len()
        );
        report(
            "the optional admission (the enqueue)",
            &mut admission_samples,
        );
        drop(session);
        let _ = std::fs::remove_dir_all(&dir);

        // 9e — THE T1 DEACTIVATION TICK + THE REQUIRED CANONICAL
        // CONFIRMATION: the far-boundary cycle (the deactivation's
        // journal fsync lands INSIDE the tick; the confirmation is
        // the worker-backed canonical commit with its readback).
        let dir = campaign_dir("orchestration-commit");
        let location = found_benchmark_store(&dir, false);
        let (spine, director) = scenario_pair(150, &near);
        let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
        let mut session =
            OrchestratedSession::open(spine, director, &location, journal, "b5-bench").unwrap();
        let mut deactivation_tick_samples = Vec::new();
        let mut commit_samples = Vec::new();
        for _ in 0..10 {
            let start = Instant::now();
            session.tick(&far_presence).unwrap();
            deactivation_tick_samples.push(start.elapsed());
            let start = Instant::now();
            session.commit_canonical().unwrap();
            commit_samples.push(start.elapsed());
            // The reactivation + the accrual (the next cycle's
            // scratch — the C3-F2-proven cadence).
            session.tick(&near_presence).unwrap();
            for _ in 0..100 {
                session.tick(&near_presence).unwrap();
            }
        }
        report(
            "the T1 deactivation tick (the journal fsync inside the tick)",
            &mut deactivation_tick_samples,
        );
        report(
            "the required canonical confirmation (the worker-backed commit)",
            &mut commit_samples,
        );

        // 9f — THE MAINTENANCE BARRIER/DRAIN (the full gate under the
        // real lease — the probe path, no compound).
        let mut barrier_samples = Vec::new();
        for _ in 0..10 {
            let start = Instant::now();
            session.debug_maintenance_probe_for_test(|_| ()).unwrap();
            barrier_samples.push(start.elapsed());
        }
        report("the maintenance barrier/drain", &mut barrier_samples);

        // 9g — THE GRACEFUL CLOSE on this campaign (the full drain +
        // the settlement + the final publication + the lifecycle
        // checkpoint + the join).
        let start = Instant::now();
        let close_report = session.close();
        let close_elapsed = start.elapsed();
        assert!(close_report.failure.is_none(), "{:?}", close_report.failure);
        println!(
            "  the graceful close: {:.3} ms (n=1 — the full close sequence)",
            close_elapsed.as_secs_f64() * 1e3
        );
        let _ = std::fs::remove_dir_all(&dir);

        // 9h — THE ORCHESTRATED MAINTENANCE COMPOUNDS (the worker-
        // backed equivalents of section 8's crossing-level numbers:
        // the two-checkpoint + rotation + reclamation campaign, fresh
        // per sample).
        let mut checkpoint_samples = Vec::new();
        let mut rotation_samples = Vec::new();
        let mut reclamation_samples = Vec::new();
        for _ in 0..5u64 {
            let dir = campaign_dir("orchestration-retention");
            let location = found_benchmark_store(&dir, false);
            let (spine, director) = scenario_pair(150, &near);
            let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
            let mut session =
                OrchestratedSession::open(spine, director, &location, journal, "b5-bench").unwrap();
            // The prelude: the deactivation + the confirmation + the
            // reactivation + the accrual (the orchestrated path's
            // eligibility requirement — the applied identities must
            // be confirmed for the compaction).
            session.tick(&far_presence).unwrap();
            session.commit_canonical().unwrap();
            session.tick(&near_presence).unwrap();
            for _ in 0..100 {
                session.tick(&near_presence).unwrap();
            }
            // RANGE 1 — TWO publications (the section-8 timing loop's
            // workload shape, for the workload-equivalent comparison;
            // the prelude's confirmed cycle is the orchestrated
            // fixture's inherent extra).
            for _ in 0..2 {
                session.tick(&near_presence).unwrap();
                for _ in 0..20 {
                    session.tick(&near_presence).unwrap();
                }
                let handle = session
                    .submit_publication(session.capture_snapshot().unwrap())
                    .unwrap();
                let _ = session.wait_publication(handle).unwrap();
            }
            let start = Instant::now();
            session.publish_checkpoint(10).unwrap();
            checkpoint_samples.push(start.elapsed());
            // RANGE 2 — two publications (the prior MK is the range
            // boundary — the range publications are required).
            for _ in 0..2 {
                session.tick(&near_presence).unwrap();
                for _ in 0..20 {
                    session.tick(&near_presence).unwrap();
                }
                let handle = session
                    .submit_publication(session.capture_snapshot().unwrap())
                    .unwrap();
                let _ = session.wait_publication(handle).unwrap();
            }
            session.publish_checkpoint(10).unwrap();
            // One outstanding journal transaction — the rotation's
            // survivor set.
            session.tick(&far_presence).unwrap();
            let start = Instant::now();
            session.rotate_journal().unwrap();
            rotation_samples.push(start.elapsed());
            let start = Instant::now();
            session.reclaim_checkpoint(1).unwrap();
            reclamation_samples.push(start.elapsed());
            drop(session);
            let _ = std::fs::remove_dir_all(&dir);
        }
        report(
            "the orchestrated checkpoint (the lease + compact + SK + MK)",
            &mut checkpoint_samples,
        );
        report(
            "the orchestrated rotation (the lease + the survivor re-anchor + the flip)",
            &mut rotation_samples,
        );
        report(
            "the orchestrated reclamation (the lease + the audit + the deletes)",
            &mut reclamation_samples,
        );

        // 9i — THE SQLITE PARITY (the tick, the deactivation tick, the
        // required confirmation, and the close over the Database
        // provider).
        let dir = campaign_dir("orchestration-sqlite");
        let location = found_benchmark_store(&dir, true);
        let (spine, director) = scenario_pair(150, &near);
        let journal = T1Journal::open(dir.join("journal.t1")).unwrap();
        let mut session =
            OrchestratedSession::open(spine, director, &location, journal, "b5-bench").unwrap();
        for _ in 0..50 {
            session.tick(&near_presence).unwrap();
        }
        let mut sqlite_ticks = Vec::new();
        for _ in 0..100 {
            let start = Instant::now();
            session.tick(&near_presence).unwrap();
            sqlite_ticks.push(start.elapsed());
        }
        report(
            "the SQLite authoritative tick (idle worker)",
            &mut sqlite_ticks,
        );
        let start = Instant::now();
        session.tick(&far_presence).unwrap();
        let sqlite_deactivation = start.elapsed();
        let start = Instant::now();
        session.commit_canonical().unwrap();
        let sqlite_commit = start.elapsed();
        println!(
            "  the SQLite T1 deactivation tick: {:.3} ms; the SQLite required confirmation: {:.3} ms (n=1 each)",
            sqlite_deactivation.as_secs_f64() * 1e3,
            sqlite_commit.as_secs_f64() * 1e3
        );
        let start = Instant::now();
        let close_report = session.close();
        let sqlite_close = start.elapsed();
        assert!(close_report.failure.is_none(), "{:?}", close_report.failure);
        println!(
            "  the SQLite graceful close: {:.3} ms (n=1)",
            sqlite_close.as_secs_f64() * 1e3
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    println!(
        "  PP-B4 boundaries: the optional publication path is asynchronous for the authority thread (a bounded enqueue; a typed Busy at the boundary); T1 durability and protected maintenance retain their accepted SYNCHRONOUS boundaries — never a claim that all persistence is nonblocking; the 16.667 ms frame budget is the comparison reference, never a guaranteed maximum"
    );

    // The storage summary.
    println!(
        "storage: the session snapshot object {save_len} B AT 150 TICKS (lifetime-growing — see the sweep above); the journal {journal_min}-{journal_max} B for the first capture-bearing transaction on a fresh campaign"
    );

    // The frame budgets (the interpretation with the full methodology
    // and the pod-vs-rig boundary lives in the Batch 5 record).
    println!(
        "frame budgets: 60 FPS = 16.667 ms / 144 FPS = 6.944 ms / 240 FPS = 4.167 ms per frame"
    );
    println!(
        "interpretation: docs/STAGE5-B5-LIFECYCLE-RECORD.md (the measured-vs-budget analysis; pod results, not physical-rig evidence)"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&dir);
}

/// S6-C1: the instrumentation-overhead measurement (the accepted
/// measurement plan §5): (1) the produce-path cost — sample
/// construction plus the bounded nonblocking enqueue — over 10,000
/// samples; (2) the SAME deterministic headless workload (600 ticks,
/// the deliberate action at tick 300) run twice, evidence collection
/// disabled versus enabled, compared for wall time and
/// logical-fingerprint equality. Pod results — observational, never
/// physical-rig evidence, never CI pass/fail thresholds.
pub fn run_instrumentation_overhead() {
    println!(
        "S6-C1 instrumentation overhead (POD results — observational, not physical-rig evidence)"
    );

    // 1. The produce path — construction + the nonblocking enqueue.
    //    The flood's channel is sized for the full 10,000 samples
    //    (the default 8,192 bound is sized for real-run production
    //    rates; a first draft at the default bound overflowed and the
    //    verdict failed exactly as designed — 1,764 samples counted
    //    lost, none silent — the lossless micro measurement sizes its
    //    own bound instead).
    let micro_root = campaign_dir("s6c1-overhead-micro");
    let mut micro_config = EvidenceConfig::new(micro_root.join("evidence"), "s6c1-micro");
    micro_config.channel_capacity = 10_000;
    let collector = EvidenceCollector::start(micro_config).expect("the evidence collector starts");
    let mut produce_costs = Vec::with_capacity(10_000);
    for tick in 0..10_000u64 {
        let record = EvidenceRecord::Tick(minimal_tick_sample(tick));
        let start = Instant::now();
        collector.produce(record);
        produce_costs.push(start.elapsed());
    }
    let micro = collector.finalize();
    assert!(micro.is_complete(), "{micro:?}");
    report(
        "the sample produce path (construct + enqueue)",
        &mut produce_costs,
    );
    println!(
        "  micro run: {} samples · verdict complete = {}",
        micro.total_samples(),
        micro.is_complete()
    );
    let _ = std::fs::remove_dir_all(&micro_root);

    // 2. The same deterministic workload, evidence off vs on — each
    // arm measured around its own fresh campaign directory.
    let plain_root = campaign_dir("s6c1-overhead-plain");
    let start = Instant::now();
    let plain = run_persist_demo_headless(overhead_options(&plain_root, None))
        .expect("the plain headless run");
    let plain_wall = start.elapsed();

    let evidence_root = campaign_dir("s6c1-overhead-evidence");
    let start = Instant::now();
    let instrumented = run_persist_demo_headless(overhead_options(
        &evidence_root,
        Some(EvidenceConfig::new(
            evidence_root.join("evidence"),
            "s6c1-overhead",
        )),
    ))
    .expect("the instrumented headless run");
    let instrumented_wall = start.elapsed();

    assert_eq!(
        plain.closed_fingerprint, instrumented.closed_fingerprint,
        "instrumentation must not change logical state"
    );
    assert_eq!(plain.closed_at_tick, instrumented.closed_at_tick);
    let outcome = instrumented
        .evidence
        .as_ref()
        .expect("the evidence verdict");
    assert!(outcome.is_complete(), "{outcome:?}");
    println!(
        "  run wall (600 ticks + the action): plain {:.1} ms vs instrumented {:.1} ms",
        plain_wall.as_secs_f64() * 1e3,
        instrumented_wall.as_secs_f64() * 1e3
    );
    println!(
        "  evidence: {} samples · verdict complete = {}",
        outcome.total_samples(),
        outcome.is_complete()
    );
    println!(
        "  logical fingerprints identical: {} (closed at tick {})",
        instrumented.closed_fingerprint,
        instrumented.closed_at_tick.as_u64()
    );
    println!(
        "  limitations: single-process pod timing, dev-profile scheduler noise, one repetition \
         each; the authoritative overhead evidence is the owner's rig (S6-C4); never CI \
         pass/fail thresholds"
    );
    let _ = std::fs::remove_dir_all(&plain_root);
    let _ = std::fs::remove_dir_all(&evidence_root);
}

fn minimal_tick_sample(tick: u64) -> TickSample {
    TickSample {
        tick,
        wall_offset_ms: 0.0,
        tick_ms: 0.0,
        ticks_due_total: tick,
        outstanding_ticks: 0,
        executed: tick,
        scheduled: tick,
        late_arrivals: 0,
        late_executions: 0,
        blocked_transitions: 0,
        invalid_dropped: 0,
        budget_exhausted_ticks: 0,
        near_pending: 0,
        far_pending: 0,
        blocked: 0,
        held: 0,
        near_executed_total: 0,
        far_executed_total: 0,
        near_backlogged_ticks: 0,
        far_backlogged_ticks: 0,
        processed_through: None,
        max_executed_due: None,
        transitioned_regions: Vec::new(),
    }
}

fn overhead_options(root: &Path, evidence: Option<EvidenceConfig>) -> DemoOptions {
    DemoOptions {
        campaign_dir: root.to_path_buf(),
        fps: 60,
        present: engine_renderer::PresentModePreference::Auto,
        frames: None,
        overlay: true,
        validation: engine_renderer::ValidationPreference::Auto,
        headless: true,
        headless_ticks: 600,
        action_at: Some(300),
        poison_at_tick: None,
        evidence,
        evidence_fail_writes: false,
        scenario: None,
        scenario_seed: None,
        soak_wall_seconds: None,
        stabilization_deadline_secs: None,
    }
}
