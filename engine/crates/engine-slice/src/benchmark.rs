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
use engine_persist::{CommitOutcome, LocalFileMedium, SqliteMedium, T1Journal};

use crate::crossing::{SessionCrossing, restore_session, save_session};
use crate::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use crate::region::{RegionConfig, RegionDirector};
use crate::store::StoreLocation;

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
    let max = samples[n - 1].as_secs_f64() * 1e3;
    println!(
        "  {name}: min {min:.3} / median {median:.3} / p95 {p95:.3} / max {max:.3} ms (n={n})"
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
