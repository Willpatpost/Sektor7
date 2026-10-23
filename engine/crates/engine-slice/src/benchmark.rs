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
//! - **Storage growth** (the save bytes, the journal bytes per first
//!   transaction, the store bytes per publication).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine_core::{PresenceSample, Spine};
use engine_persist::{CommitOutcome, LocalFileMedium, T1Journal};

use crate::crossing::{SessionCrossing, save_session};
use crate::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use crate::region::{RegionConfig, RegionDirector};

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

    // The storage summary.
    println!(
        "storage: the session snapshot object {save_len} B; the journal {journal_min}-{journal_max} B for the first capture-bearing transaction on a fresh campaign"
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
