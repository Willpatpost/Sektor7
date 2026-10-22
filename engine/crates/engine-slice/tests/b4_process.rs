//! Stage 5 Batch 4: the REAL process-kill matrix at the full stack —
//! the GO's "mid-install process termination". A child process
//! (this binary re-exec'd, the B2/B3 harness pattern) founds the
//! session, publishes the verified snapshot, journals the
//! deactivation transaction, then HARD-EXITS between durability and
//! the install — the exact §5.5.2 crash window. The parent — an
//! independent process — reconstructs the session from disk alone
//! (the chain snapshot + the journal) and proves the deactivation
//! re-installs from the plan through the id-exact held
//! reconstruction, the captures land in the ledger, and the pending
//! T1 re-drive confirms canonically.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::{CommitOutcome, ConsequenceId, T1Journal};
use engine_slice::crossing::SessionCrossing;
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};

const ROLE_ENV: &str = "S7_B4_CHILD_ROLE";
const STORE_ENV: &str = "S7_B4_STORE";
const JOURNAL_ENV: &str = "S7_B4_JOURNAL";
const TEST_NAME: &str = "child_death_between_journal_and_install_recovers_from_disk";

fn near_region_4() -> PresenceSample {
    PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    }
}

/// The real scenario pair (the crossing suite's fixture, local to
/// this file): the Lod-mode spine plus the director, driven through
/// the driver's own per-tick sequence.
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

/// The child's role: found the session, publish the snapshot, journal
/// region 4's deactivation (the capture-bearing transaction), then
/// hard-exit INSIDE the phase-4/phase-5 gap — the journal transaction
/// is durably committed, the install never runs, the process dies
/// with its in-memory state. No drops, no publication.
fn child_role(store: PathBuf, journal_path: PathBuf) -> ! {
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    let journal = T1Journal::open(&journal_path).unwrap();
    let mut crossing =
        SessionCrossing::open(&store, journal, "process-campaign").expect("the crossing opens");
    crossing
        .publish_snapshot(&spine, &director)
        .expect("the snapshot publishes");
    // The mid-install kill: the gap runs between the durable receipt
    // and the install — hard-exit there (exit 6, the B3 harness's
    // child-death convention).
    let now = spine.clock();
    let _ = crossing.debug_deactivate_around_install(&mut spine, &mut director, 4, now, |_, _| {
        std::process::exit(6);
    });
    unreachable!("the child exits inside the gap");
}

#[test]
fn child_death_between_journal_and_install_recovers_from_disk() {
    if let (Ok(role), Ok(store), Ok(journal)) = (
        std::env::var(ROLE_ENV),
        std::env::var(STORE_ENV),
        std::env::var(JOURNAL_ENV),
    ) {
        assert_eq!(role, "mid-install-kill", "the only B4 child role");
        child_role(store.into(), journal.into());
    }
    // The parent: a fresh campaign directory for the child.
    let dir = std::env::temp_dir().join(format!(
        "s7-b4-process-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = dir.join("store");
    let journal_path = dir.join("journal.t1");
    // The child re-exec: this test binary, this test only (--exact),
    // with the role environment.
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .env(ROLE_ENV, "mid-install-kill")
        .env(STORE_ENV, &store)
        .env(JOURNAL_ENV, &journal_path)
        .args(["--exact", TEST_NAME, "--nocapture"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the child spawns");
    // The bounded wait — a runaway child is killed at the deadline
    // (the harness discipline; every parental wait is bounded).
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        match child.try_wait().expect("the child is waitable") {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                child.kill().expect("the runaway is killed");
                panic!("the child did not exit within the bound");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    assert_eq!(
        status.code(),
        Some(6),
        "the child hard-exited mid-install (between durability and the install)"
    );
    // THE RECOVERY: the parent — an independent process —
    // reconstructs the session from disk alone.
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&store, &journal_path, "process-campaign")
            .expect("the recovery reconstructs from disk");
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Dormant,
        "the deactivation re-installed from the journal's plan"
    );
    let checkpoint = recovered_director.states()[4]
        .checkpoint
        .as_ref()
        .expect("the checkpoint reconstructed");
    assert_eq!(checkpoint.entries.len(), 125, "region 4's NPC chains");
    assert!(
        recovered_director.ledger().pending_total() > 0,
        "the captures landed in the ledger"
    );
    // The world: the region-4 sentinels and the milestones (the
    // crossings were in the child's snapshot).
    assert!(
        recovered_spine.world().milestones().iter().any(|m| *m > 0),
        "the snapshot's crossings restored"
    );
    for npc in (4..1000).step_by(8).take(5) {
        assert_eq!(
            recovered_spine.world().positions()[npc],
            engine_slice::npc::POSITION_DISCARDED,
            "the region-4 sentinels installed"
        );
    }
    // The pending T1 re-drive: the canonical confirmation, from disk
    // alone.
    let CommitOutcome::Confirmed(receipt) = recovered_crossing.commit_canonical().unwrap() else {
        panic!("the re-drive confirms the recovered captures");
    };
    assert!(receipt.keys.contains(&ConsequenceId::new(4, 1, 4)));
    // The cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}
