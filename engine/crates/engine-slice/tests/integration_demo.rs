//! The Slice 0 integration demonstration's focused tests — the GO's
//! six verification categories concentrated on the NEW application
//! integration seam (the campaign lifecycle, the deliberate action's
//! consequence cycle, the publication lifecycle, and the fail-closed
//! boundaries), reusing the accepted PP-B4 machinery underneath
//! rather than duplicating its failure matrix. Test 6 (regression
//! preservation) is the full workspace run plus the pinned
//! fingerprint/save self-tests — unchanged by this batch and re-run
//! by the delivery gates.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use engine_persist::CanonicalMedium;
use engine_slice::persist_demo::{
    self, CampaignPaths, DEMO_PRESENCE, DemoOptions, EXCURSION_TICKS, PersistDemo,
    PublicationState, demo_scenario, nearest_region, region_antipode, region_credit_sum,
    region_signal_sum,
};
use engine_slice::region::stage4_state_fingerprint_hex;
use engine_slice::sim::SimConfig;
use engine_slice::store::{SessionStore, StoreLocation};

/// A unique, self-cleaning campaign directory under the system temp
/// root (the house TestDir pattern — test saves never touch the
/// tracked repository).
struct TestDir {
    root: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "s7-integration-demo-{name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("the campaign directory creates");
        TestDir { root }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        // Best-effort — a failed remove never fails a test.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn headless_options(dir: &TestDir, ticks: u64, action_at: Option<u64>) -> DemoOptions {
    DemoOptions {
        campaign_dir: dir.root.clone(),
        fps: 60,
        present: engine_renderer::PresentModePreference::Auto,
        frames: None,
        overlay: true,
        validation: engine_renderer::ValidationPreference::Auto,
        headless: true,
        headless_ticks: ticks,
        action_at,
    }
}

/// The chain head read through a fresh store handle (the oracle
/// discipline — the close's verified final revision must BE the head).
fn oracle_head(dir: &TestDir) -> Option<engine_persist::Hash> {
    let paths = CampaignPaths::new(&dir.root);
    let location = StoreLocation::Database(paths.database);
    let mut store = SessionStore::open(&location).expect("the store re-opens");
    store.head().expect("the head reads")
}

// ------------------------------------------------------- GO tests 1 + 2

/// The deliberate action enters the authoritative consequence path
/// and produces the observable world change; the resulting
/// consequence is confirmed, the session closed, a fresh session
/// recovered from storage, and the exact durable outcome restored.
#[test]
fn the_deliberate_action_persists_through_close_and_recovery() {
    let dir = TestDir::new("lifecycle");
    // CYCLE 1 — a fresh campaign: found (with the verified recovery
    // anchor), run, act at tick 100 (the region's NPCs have accrued
    // uncommitted milestone signals by then), confirm, close.
    let first = persist_demo::run_persist_demo_headless(headless_options(&dir, 240, Some(100)))
        .expect("the fresh headless cycle");
    assert!(first.launch.founded, "the first launch founds the campaign");
    assert!(
        first.launch.anchor_revision.is_some(),
        "the recovery anchor published and verified at open"
    );
    let action = first.action.as_ref().expect("the action record");
    assert!(
        action.signals_before > 0,
        "the region had accrued uncommitted consequential signals"
    );
    let confirmation = action
        .confirmed
        .as_ref()
        .expect("the excursion's capture confirmed canonically");
    assert!(
        confirmation.keys > 0,
        "the confirmed receipt carries the consequence keys"
    );
    assert_eq!(
        action.credits_after,
        Some(action.credits_before + action.signals_before),
        "the reactivation applied the captured signals as exactly-once credits"
    );
    assert!(
        matches!(
            first.action_publication,
            Some(PublicationState::Accepted { .. })
        ),
        "the post-action snapshot verified canonically: {:?}",
        first.action_publication
    );
    assert!(
        first.close.failure.is_none(),
        "the graceful close completed: {:?}",
        first.close.failure
    );
    let final_revision = first
        .close
        .final_revision
        .expect("the verified final revision");
    assert!(
        first.close.lifecycle_checkpoint,
        "the SQLite WAL TRUNCATE lifecycle checkpoint"
    );
    assert!(
        first.close.required_settled,
        "the required confirmations settled"
    );
    assert_eq!(
        oracle_head(&dir),
        Some(final_revision),
        "the chain head equals the close's verified final revision"
    );

    // CYCLE 2 — relaunch against the same directory: recovery of the
    // committed consequence and the authoritative state (checked
    // against the durable state, not a message — the twin test below
    // proves the exact equality).
    let second = persist_demo::run_persist_demo_headless(headless_options(&dir, 60, None))
        .expect("the recovery cycle");
    assert!(
        !second.launch.founded,
        "the second launch recovers — never refounds, never overwrites"
    );
    assert_eq!(
        second.launch.recovered_at_tick,
        Some(first.closed_at_tick),
        "the recovery restores exactly the closed state's clock"
    );
    assert!(
        second.close.failure.is_none(),
        "the recovered campaign closes cleanly: {:?}",
        second.close.failure
    );
}

// ---------------------------------------------------------- GO test 3

/// Retries, relaunch, and repeated recovery cannot cause the single
/// action to be committed twice — the persisted pair's full
/// continuation ends EXACTLY at the never-persisted twin's state (the
/// house twin discipline: the same scenario, the same presence
/// schedule tick-for-tick including the action's excursion, no
/// persistence at all).
#[test]
fn the_recovered_continuation_matches_the_never_persisted_twin() {
    let dir = TestDir::new("twin");
    let (ticks1, action_at, ticks2) = (240u64, 100u64, 60u64);
    let total = ticks1 + ticks2;

    let first =
        persist_demo::run_persist_demo_headless(headless_options(&dir, ticks1, Some(action_at)))
            .expect("cycle 1");
    assert!(first.launch.founded);
    let second = persist_demo::run_persist_demo_headless(headless_options(&dir, ticks2, None))
        .expect("cycle 2 (the recovery)");
    assert_eq!(
        second.launch.recovered_at_tick,
        Some(first.closed_at_tick),
        "the recovery resumes from exactly the closed clock"
    );

    // THE TWIN: never persisted, byte-identical logical work — the
    // same fixed presence, with the action's excursion replicated
    // tick-for-tick (the same antipodal override window).
    let timing = SimConfig::default();
    let mut twin = demo_scenario(&timing, &[DEMO_PRESENCE]).expect("the twin scenario");
    let target_region = nearest_region(twin.1.config(), &DEMO_PRESENCE);
    let mut twin_fingerprint_at_cycle1 = String::new();
    let mut twin_region_at_cycle1: Option<(usize, u64, u64)> = None;
    for tick_index in 0..total {
        if tick_index == ticks1 {
            twin_fingerprint_at_cycle1 = stage4_state_fingerprint_hex(&twin.0, &twin.1);
            twin_region_at_cycle1 = Some((
                target_region,
                region_signal_sum(twin.0.world(), target_region),
                region_credit_sum(twin.0.world(), target_region),
            ));
        }
        let presence = if (action_at..action_at + EXCURSION_TICKS).contains(&tick_index) {
            region_antipode(twin.1.config(), target_region)
        } else {
            DEMO_PRESENCE
        };
        let (spine, director) = &mut twin;
        director.apply(spine, std::slice::from_ref(&presence));
        spine.run_tick();
    }

    let action = first.action.as_ref().expect("the action");
    assert_eq!(
        action.region, target_region,
        "both paths target the same region"
    );
    // Mid-point equivalence: cycle 1's closed state IS the twin at
    // tick 240.
    assert_eq!(
        first.closed_fingerprint, twin_fingerprint_at_cycle1,
        "the closed cycle-1 state equals the never-persisted twin"
    );
    // Final equivalence: the recovered continuation (cycle 2) ends
    // exactly at the twin's state — the single action's consequence
    // applied exactly once, no duplication across close/recovery, the
    // background cadence identical.
    let twin_fingerprint = stage4_state_fingerprint_hex(&twin.0, &twin.1);
    assert_eq!(
        second.closed_fingerprint, twin_fingerprint,
        "the persisted pair ends exactly at the never-persisted twin's state"
    );
    // The action region's durable counters at CYCLE 1's close match
    // the twin exactly (the recovered continuation's equality is the
    // fingerprint assertion above — the strongest form, covering
    // every counter).
    assert_eq!(
        first.closed_region, twin_region_at_cycle1,
        "the action region's durable counters at close match the twin exactly"
    );
    assert_eq!(
        second.closed_at_tick.as_u64(),
        total,
        "the campaign clock continued across the recovery without replay"
    );
}

// ---------------------------------------------------------- GO test 4

/// The demonstration cannot report safe persistence before canonical
/// confirmation — a pending publication is displayed as pending
/// (unverified), an accepted one only with the verified receipt, and
/// the non-accepted classes never label themselves saved/confirmed.
#[test]
fn pending_publications_are_never_reported_as_confirmed() {
    let dir = TestDir::new("pending");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    demo.run_headless(2).unwrap();
    demo.excursion().expect("the action");
    // The excursion window plus the return tick plus one settling
    // tick — enough for the completion refresh and the publication.
    demo.run_headless(EXCURSION_TICKS + 2).unwrap();

    // The action completed → the snapshot publication was submitted.
    // Before the application verifies the outcome the status may be
    // pending or already accepted — but pending NEVER claims
    // confirmation.
    let status = demo.status();
    let publication = status
        .publication
        .clone()
        .expect("the action's publication");
    match &publication {
        PublicationState::Pending { .. } => {
            assert!(
                status.confirmed_revision.is_none(),
                "a pending publication is never confirmed"
            );
            assert!(status.pending_publications >= 1, "the admission is tracked");
            assert!(
                publication.label().starts_with("pending"),
                "the label names itself: {}",
                publication.label()
            );
        }
        PublicationState::Accepted { revision, .. } => {
            assert_eq!(status.confirmed_revision, Some(*revision));
        }
        other => panic!("the action publication completed unexpectedly: {other:?}"),
    }

    // The completed publication verifies — the only confirmed state.
    demo.wait_current();
    let status = demo.status();
    assert!(
        matches!(status.publication, Some(PublicationState::Accepted { .. })),
        "the verified receipt: {:?}",
        status.publication
    );

    // The honest labels: a definite failure names itself, never a save.
    assert!(
        PublicationState::Failed {
            detail: "the medium refused".into()
        }
        .label()
        .contains("FAILED")
    );

    let close = demo.close();
    assert!(close.failure.is_none(), "{:?}", close.failure);
}

// ---------------------------------------------------------- GO test 5

/// A genuine failure at the application integration boundary — the
/// worker poisoned mid-session — refuses every authoritative
/// operation BEFORE the clock advances, refuses the action, and the
/// close reports the explicit failure (never a claimed save); the
/// durable state remains recoverable and a fresh session closes
/// cleanly.
#[test]
fn a_poisoned_worker_refuses_progression_and_the_close_reports_it() {
    let dir = TestDir::new("poison");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    demo.run_headless(2).unwrap();
    let clock_before = demo.session().clock();
    let steps_before = demo.session().spine().world().total_steps();
    demo.session().poison();

    assert!(
        demo.excursion().is_err(),
        "the deliberate action refuses while the worker is down"
    );
    assert!(
        demo.run_headless(1).is_err(),
        "the authoritative tick refuses through the composed gate"
    );
    assert_eq!(
        demo.session().clock(),
        clock_before,
        "the clock never advanced — no silent progression"
    );
    assert_eq!(
        demo.session().spine().world().total_steps(),
        steps_before,
        "no authoritative world change sneaks through"
    );

    let report = demo.close();
    assert!(
        report.failure.is_some(),
        "the poisoned close is an explicit failure — never a claimed save"
    );
    assert!(!report.required_settled);

    // The poison is in-memory only: the durable state is untouched and
    // a fresh recovery is coherent.
    let (mut fresh, launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    assert!(!launch.founded, "the recovery path");
    fresh.run_headless(1).unwrap();
    let close = fresh.close();
    assert!(close.failure.is_none(), "{:?}", close.failure);
}

/// The fail-closed corruption boundary (the GO §2A): a corrupt
/// campaign database fails closed with the typed diagnostic — never
/// overwritten, never silently refounded as a replacement campaign.
#[test]
fn a_corrupt_database_fails_closed_without_overwriting() {
    let dir = TestDir::new("corrupt");
    let report = persist_demo::run_persist_demo_headless(headless_options(&dir, 20, None)).unwrap();
    assert!(report.close.failure.is_none(), "{:?}", report.close.failure);

    // Tamper the database header — a definitely-invalid database file.
    let database = dir.root.join("campaign.db");
    let original = std::fs::read(&database).unwrap();
    let mut tampered = original.clone();
    for byte in &mut tampered[0..16] {
        *byte ^= 0xFF;
    }
    std::fs::write(&database, &tampered).unwrap();

    // The relaunch fails closed (the typed open/recover diagnostic) —
    // and never touches the file.
    let error = persist_demo::run_persist_demo_headless(headless_options(&dir, 5, None))
        .expect_err("the corrupt database must fail closed");
    assert!(
        error.contains("refusing to touch the durable state")
            || error.contains("recover the campaign"),
        "the failure names the fail-closed refusal: {error}"
    );
    assert_eq!(
        std::fs::read(&database).unwrap(),
        tampered,
        "the corrupt file is untouched — no overwrite, no replacement founding"
    );
    // The journal beside it is untouched too.
    assert!(dir.root.join("journal.t1").exists());
}
