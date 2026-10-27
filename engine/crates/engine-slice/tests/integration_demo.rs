//! The Slice 0 integration demonstration's focused tests — the GO's
//! verification categories at the NEW application integration seam,
//! plus the Checkpoint-1 corrective pass's regressions: HOLD-1's
//! error-path finalization, HOLD-2's absolute time accounting,
//! HOLD-3's target attribution (unrelated transitions, reactivation
//! gating, the actual consequence, the zero-capture no-op), the
//! overlapping-action admission rule, and the strengthened
//! pending-publication coverage (the required confirmation and the
//! optional publication never conflated). Test 6 (regression
//! preservation) is the full workspace run plus the pinned
//! fingerprint/save self-tests — re-run by the delivery gates.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use engine_core::PresenceSample;
use engine_persist::CanonicalMedium;
use engine_slice::npc::{NPC_COUNT, REGION_COUNT};
use engine_slice::persist_demo::{
    self, ActionPhase, CampaignPaths, DEMO_PRESENCE, DemoOptions, EXCURSION_TICKS, PersistDemo,
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
        poison_at_tick: None,
        evidence: None,
        evidence_fail_writes: false,
        scenario: None,
        scenario_seed: None,
        soak_wall_seconds: None,
        stabilization_deadline_secs: None,
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
/// consequence is confirmed, attributed to the target's own
/// identities, applied as exactly-once credits, closed, and recovered
/// exactly.
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
        action.captured_delta > 0,
        "the region had accrued uncommitted consequential signals"
    );
    let ActionPhase::Applied { keys, .. } = &action.phase else {
        panic!(
            "the action completed with the verified application: {:?}",
            action.phase
        )
    };
    assert!(
        *keys > 0,
        "the confirmed receipt carries the target's consequence keys"
    );
    assert_eq!(
        action.credits_after,
        Some(action.credits_before + action.captured_delta),
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

// ------------------------------------- GO test 4 + the corrective finding

/// The required canonical confirmation and the optional snapshot
/// publication are DISTINCT acknowledgments, never conflated: a
/// genuine nonempty capture is confirmed and applied (the receipt
/// attributed), while the optional publication separately remains
/// pending — deterministically, because the tracked state only
/// advances through the application's own poll/wait — and only the
/// verified `Accepted` receipt updates the label.
#[test]
fn pending_publications_are_never_reported_as_confirmed() {
    let dir = TestDir::new("pending");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    // A GENUINE nonempty capture: 120 ticks near region 0 first — the
    // region's citizens accrue milestone signals before the action.
    for _ in 0..120 {
        demo.run_headless(1).unwrap();
    }
    demo.excursion().expect("the action");
    for _ in 0..(EXCURSION_TICKS + 2) {
        demo.run_headless(1).unwrap();
    }

    // The action COMPLETED with a real consequence — the required
    // canonical confirmation verified and attributed:
    let action = demo.action().expect("the action");
    let ActionPhase::Applied { keys, .. } = &action.phase else {
        panic!("the applied phase: {:?}", action.phase)
    };
    assert!(*keys > 0, "real consequence keys confirmed");
    assert_eq!(
        action.credits_after,
        Some(action.credits_before + action.captured_delta)
    );

    // ...while the OPTIONAL snapshot publication is a SEPARATE
    // acknowledgment, still pending (deterministic — no poll has
    // consumed the tracked state):
    let status = demo.status();
    assert!(
        status.confirmed_revision.is_some(),
        "the required confirmation verified"
    );
    let publication = status
        .publication
        .clone()
        .expect("the action's publication");
    assert!(
        publication.is_pending(),
        "the optional publication separately pending: {publication:?}"
    );
    assert!(status.pending_publications >= 1, "the admission is tracked");
    assert!(
        publication.label().starts_with("pending"),
        "the label names itself: {}",
        publication.label()
    );
    // A confirmed required consequence does NOT imply the optional
    // publication completed — both states coexist above (the two
    // acknowledgments proven distinct).

    // The verified receipt updates the label only on completion:
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

    let finalization = demo.finalize(None);
    assert!(
        finalization.close.failure.is_none(),
        "{:?}",
        finalization.close.failure
    );
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

    let finalization = demo.finalize(None);
    assert!(
        finalization.close.failure.is_some(),
        "the poisoned close is an explicit failure — never a claimed save"
    );
    assert!(!finalization.close.required_settled);

    // The poison is in-memory only: the durable state is untouched and
    // a fresh recovery is coherent.
    let (mut fresh, launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    assert!(!launch.founded, "the recovery path");
    fresh.run_headless(1).unwrap();
    let finalization = fresh.finalize(None);
    assert!(
        finalization.close.failure.is_none(),
        "{:?}",
        finalization.close.failure
    );
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

// ------------------------------------------------- HOLD-1: error-path close

/// A controlled application-boundary failure after the session opened
/// (the worker poisoned mid-run through the test seam) exits THROUGH
/// the single finalization site: the original error preserved, the
/// close attempted and its outcome observable, the failed close never
/// reported as success, and the recovery consistent with the actual
/// durable state (no durability asserted beyond what was confirmed).
#[test]
fn an_application_failure_after_open_still_finalizes_the_close_once() {
    let dir = TestDir::new("finalization");
    let mut options = headless_options(&dir, 240, Some(100));
    options.poison_at_tick = Some(120); // the controlled failure
    let report = persist_demo::run_persist_demo_headless(options)
        .expect("the run reports through the finalization rather than bypassing it");

    // 1. The original application error is preserved:
    let failure = report
        .application_failure
        .as_ref()
        .expect("the application failure preserved");
    assert!(failure.contains("tick 120 refused"), "{failure}");

    // 2 + 3. The finalization ran (the single close report present —
    // the driver is consumed by it, so it cannot run twice) and its
    // outcome is observable: the poisoned worker's close FAILED —
    // reported as a failure, never as success.
    assert!(
        report.close.failure.is_some(),
        "the failed close is a reported failure"
    );
    assert!(!report.close.required_settled);
    assert!(
        report.close.final_revision.is_none(),
        "no final snapshot is claimed over the failed drain"
    );

    // 5. The recovery is consistent with the ACTUAL durable state:
    // the failed run's ticks after the last verified snapshot are not
    // claimed — the recovery restores at most the durably represented
    // state.
    let (mut fresh, launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    assert!(!launch.founded, "the recovery path");
    assert!(
        launch
            .recovered_at_tick
            .expect("the restored clock")
            .as_u64()
            <= 120,
        "the recovery restores at most the durably represented state"
    );
    fresh.run_headless(1).unwrap();
    let finalization = fresh.finalize(None);
    assert!(
        finalization.close.failure.is_none(),
        "{:?}",
        finalization.close.failure
    );
}

// ---------------------------------------- HOLD-2: absolute time accounting

/// The reported time-accounting equation is mathematically correct
/// for a founded campaign, a recovered campaign, and a recovered
/// campaign with retained catch-up obligations (the long/stalled
/// graphical frame is the same retained-obligation mechanism): the
/// absolute due == campaign clock + outstanding, with the recovered
/// starting clock included.
#[test]
fn the_time_accounting_equation_holds_across_founding_and_recovery() {
    let dir = TestDir::new("accounting");

    // FOUNDING: 240 headless ticks — due == clock, nothing retained.
    let first = persist_demo::run_persist_demo_headless(headless_options(&dir, 240, None)).unwrap();
    assert_eq!(first.closed_at_tick.as_u64(), 240);
    assert_eq!(first.closed_ticks_due, 240, "founding: due == clock");
    assert_eq!(first.closed_outstanding, 0);

    // RECOVERY: 60 further ticks — the ABSOLUTE equation (the original
    // defect displayed `60 = 300 + 0` here).
    let second = persist_demo::run_persist_demo_headless(headless_options(&dir, 60, None)).unwrap();
    assert_eq!(
        second.launch.recovered_at_tick.map(|tick| tick.as_u64()),
        Some(240)
    );
    assert_eq!(second.closed_at_tick.as_u64(), 300);
    assert_eq!(
        second.closed_ticks_due, 300,
        "recovery: due == recovered-start 240 + session 60 == clock 300 + outstanding 0"
    );

    // RECOVERED WITH OUTSTANDING: a wall-clock burst beyond the
    // per-frame catch-up bound retains a REPORTED obligation, the
    // equation holds at every step, and the drain loses nothing.
    let (mut demo, launch) = PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    assert_eq!(
        launch.recovered_at_tick.map(|tick| tick.as_u64()),
        Some(300)
    );
    // 2 seconds at the 10 Hz demo rate = 20 ticks due in one advance;
    // the 8-per-frame catch-up bound executes 8 and retains 12.
    demo.advance(Duration::from_millis(2_000)).unwrap();
    assert_eq!(
        demo.outstanding_ticks(),
        12,
        "the burst beyond the bound is retained, never discarded"
    );
    assert_eq!(
        demo.ticks_due_total(),
        demo.session().clock().as_u64() + u64::from(demo.outstanding_ticks()),
        "the conservation invariant holds with the retained obligation"
    );
    assert_eq!(
        demo.ticks_due_total(),
        300 + 20,
        "the absolute due includes the recovered starting clock"
    );
    // Drain the retained obligation across further frames — the
    // equation holds throughout and nothing is silently discarded.
    while demo.outstanding_ticks() > 0 {
        demo.advance(Duration::from_millis(700)).unwrap();
        assert_eq!(
            demo.ticks_due_total(),
            demo.session().clock().as_u64() + u64::from(demo.outstanding_ticks()),
            "the conservation invariant holds at every drain step"
        );
    }
    assert_eq!(
        demo.ticks_due_total(),
        demo.session().clock().as_u64(),
        "fully drained: due == clock"
    );
    let finalization = demo.finalize(None);
    assert!(
        finalization.close.failure.is_none(),
        "{:?}",
        finalization.close.failure
    );
}

// ------------------------------------- HOLD-3 A + C: target attribution

/// The action claims ONLY its target region's consequences: with
/// unrelated regions capturing and confirming in the SAME
/// deactivation batch, the action's attributed keys are exactly the
/// target's own identities, and the applied credits equal the
/// TARGET's captured delta — never the unrelated regions'.
#[test]
fn an_action_claims_only_its_target_regions_consequences() {
    let dir = TestDir::new("attribution");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    // 450 ticks at a presence that keeps region 0 Full AND its
    // neighbor region 1 Simplified: BOTH accrue milestone signals —
    // the excursion's same-tick deactivations capture BOTH, so the
    // confirmed receipt carries unrelated identities the action must
    // NOT claim.
    demo.set_presence(vec![PresenceSample {
        position: (100.0, 24.0),
        velocity: (0.0, 0.0),
    }]);
    for _ in 0..450 {
        demo.run_headless(1).unwrap();
    }
    let world = demo.session().spine().world();
    let target_signals = region_signal_sum(world, 0);
    let unrelated_signals = region_signal_sum(world, 1);
    assert!(target_signals > 0, "the target accrued signals");
    assert!(
        unrelated_signals > 0,
        "the unrelated regions accrued signals too (the same-batch case is real)"
    );
    let expected_keys = (0..NPC_COUNT)
        .filter(|npc| npc % REGION_COUNT == 0 && world.pending_signals()[*npc] > 0)
        .count();
    let record = demo.excursion().expect("the action");
    assert_eq!(record.region, 0, "the target is the presence's region");
    for _ in 0..(EXCURSION_TICKS + 2) {
        demo.run_headless(1).unwrap();
    }
    let action = demo.action().expect("the action");
    let ActionPhase::Applied { keys, .. } = &action.phase else {
        panic!("the applied phase: {:?}", action.phase)
    };
    assert_eq!(
        *keys, expected_keys,
        "only the target's consequence identities are attributed — the unrelated \
         regions' same-batch captures are excluded"
    );
    assert_eq!(
        action.credits_after,
        Some(action.credits_before + action.captured_delta),
        "the applied credits equal the TARGET's captured delta ({target_signals})"
    );
    // The unrelated region's captures applied to ITS OWN NPCs at the
    // reactivation — background work, never claimed by the action:
    let world = demo.session().spine().world();
    assert!(
        region_credit_sum(world, 1) > 0,
        "the unrelated region's own credits applied separately"
    );
    let finalization = demo.finalize(None);
    assert!(
        finalization.close.failure.is_none(),
        "{:?}",
        finalization.close.failure
    );
}

// ----------------------------------- HOLD-3 B: reactivation gating

/// The action cannot claim completed credit application while its
/// target has not reactivated: with the presence kept far after the
/// excursion (the player walked away), the action stays honestly
/// `AwaitingReactivation` — durably confirmed, credits NOT applied —
/// and completes only when the target actually reactivates, with the
/// applied change verified against the captured delta.
#[test]
fn an_action_awaits_its_targets_reactivation_before_claiming_credits() {
    let dir = TestDir::new("reactivation");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    for _ in 0..120 {
        demo.run_headless(1).unwrap();
    }
    let record = demo.excursion().expect("the action");
    assert_eq!(record.region, 0);
    assert!(record.captured_delta > 0, "a nonempty capture");

    // Run the excursion out, then keep the presence FAR from the
    // target — the return tick does not reactivate it:
    for _ in 0..EXCURSION_TICKS {
        demo.run_headless(1).unwrap();
    }
    let far = region_antipode(demo.session().director().config(), 0);
    demo.set_presence(vec![far]);
    demo.run_headless(1).unwrap(); // the return tick under a far presence

    let action = demo.action().expect("the action");
    assert!(
        matches!(action.phase, ActionPhase::AwaitingReactivation { keys, .. } if keys > 0),
        "durably confirmed but NOT applied while the target stays dormant: {:?}",
        action.phase
    );
    assert!(
        action.credits_after.is_none(),
        "no credit application claimed before the reactivation"
    );

    // The player returns — the target reactivates and the application
    // completes, verified against the captured delta:
    demo.set_presence(vec![DEMO_PRESENCE]);
    demo.run_headless(2).unwrap();
    let action = demo.action().expect("the action");
    assert!(
        matches!(action.phase, ActionPhase::Applied { .. }),
        "{:?}",
        action.phase
    );
    assert_eq!(
        action.credits_after,
        Some(action.credits_before + action.captured_delta)
    );
    let finalization = demo.finalize(None);
    assert!(
        finalization.close.failure.is_none(),
        "{:?}",
        finalization.close.failure
    );
}

// ------------------------------------------- HOLD-3 D: the zero-capture case

/// An excursion before milestone signals have accumulated completes
/// honestly as a zero-consequence no-op — not indefinitely labeled
/// pending, and no nonexistent canonical confirmation claimed (the
/// empty-scratch deactivation journals nothing, by the accepted rule).
#[test]
fn a_zero_capture_excursion_completes_as_an_honest_no_op() {
    let dir = TestDir::new("zero-capture");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    // Two ticks — no milestone signals have accumulated yet.
    demo.run_headless(2).unwrap();
    let world = demo.session().spine().world();
    assert_eq!(region_signal_sum(world, 0), 0, "no signals accrued yet");
    demo.excursion().expect("the action");
    for _ in 0..(EXCURSION_TICKS + 2) {
        demo.run_headless(1).unwrap();
    }
    let action = demo.action().expect("the action");
    assert!(
        matches!(action.phase, ActionPhase::ZeroCapture),
        "completes honestly as a no-op — not pending, not a false confirmation: {:?}",
        action.phase
    );
    assert_eq!(
        action.credits_after,
        Some(action.credits_before),
        "the observed credits are unchanged"
    );
    let finalization = demo.finalize(None);
    assert!(
        finalization.close.failure.is_none(),
        "{:?}",
        finalization.close.failure
    );
}

// -------------------------------------- the overlapping-action admission

/// One deliberate action at a time: a press during an active or
/// unsettled excursion is refused WITHOUT changing the active
/// action's target or attribution; the first action's capture and
/// reward apply exactly once; a valid subsequent action is accepted
/// and reported independently once the first settles.
#[test]
fn overlapping_space_presses_are_refused_without_overwriting() {
    let dir = TestDir::new("overlap");
    let (mut demo, _launch) =
        PersistDemo::open_or_recover(&dir.root, SimConfig::default()).unwrap();
    for _ in 0..120 {
        demo.run_headless(1).unwrap();
    }
    let first = demo.excursion().expect("the first action");
    assert_eq!(first.region, 0);

    // The press during the ACTIVE excursion — refused; the first
    // action's target and attribution preserved:
    assert!(
        demo.excursion().is_err(),
        "the overlapping press is refused"
    );
    let active = demo.action().expect("the active action");
    assert_eq!(active.region, first.region);
    assert_eq!(active.started_at, first.started_at);

    // Run the excursion out but keep the target dormant (unsettled):
    for _ in 0..EXCURSION_TICKS {
        demo.run_headless(1).unwrap();
    }
    let far = region_antipode(demo.session().director().config(), 0);
    demo.set_presence(vec![far]);
    demo.run_headless(1).unwrap();
    assert!(
        demo.excursion().is_err(),
        "the press while unsettled is refused"
    );
    let unsettled = demo.action().expect("the unsettled action");
    assert_eq!(
        unsettled.started_at, first.started_at,
        "no overwrite of the first action's provenance"
    );

    // The player returns — the first action completes with its own
    // attribution (exactly one capture, exactly one reward):
    demo.set_presence(vec![DEMO_PRESENCE]);
    demo.run_headless(2).unwrap();
    let completed = demo.action().expect("the completed action");
    assert!(
        matches!(completed.phase, ActionPhase::Applied { .. }),
        "{:?}",
        completed.phase
    );
    assert_eq!(
        completed.credits_after,
        Some(completed.credits_before + completed.captured_delta),
        "the first action's reward applied exactly once"
    );

    // A valid SUBSEQUENT action is accepted independently once the
    // first settles (an honest zero-capture no-op here — the scratch
    // was captured by the first):
    let second = demo.excursion().expect("the subsequent action");
    assert!(
        second.started_at > first.started_at,
        "an independent record"
    );
    assert_eq!(second.region, 0);
    for _ in 0..(EXCURSION_TICKS + 2) {
        demo.run_headless(1).unwrap();
    }
    let second_final = demo.action().expect("the second action");
    assert!(
        matches!(second_final.phase, ActionPhase::ZeroCapture),
        "the second action completes independently: {:?}",
        second_final.phase
    );
    assert_eq!(
        second_final.started_at, second.started_at,
        "the second action's own record preserved"
    );
    let finalization = demo.finalize(None);
    assert!(
        finalization.close.failure.is_none(),
        "{:?}",
        finalization.close.failure
    );
}
