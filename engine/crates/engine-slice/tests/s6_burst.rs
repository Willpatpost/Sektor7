//! S6-C2 finite-burst stress and no-silent-drop verification (Stage 6
//! Checkpoint 2, the owner GO of 2026-10-27): the B1–B8 matrix over the
//! REAL orchestrated session — every injection through the accepted
//! public boundary `OrchestratedSession::schedule_event`, every verdict
//! through an INDEPENDENT identity-level oracle.
//!
//! **The burst cohort:** finite, deterministic, and clearly identified —
//! payload `magic(16) | cohort(16) | index(32)` with `BURST_MAGIC`
//! (`0x5336`), distinct from the routine-chain tag `ROUTINE_MAGIC`
//! (`0x5E47`). The NPC handler's region-LOD path decodes a payload as a
//! routine chain ONLY when the top-16 tag equals `ROUTINE_MAGIC`
//! (`routine_payload_parts`), so a burst payload is foreign: counted in
//! the spine's ledger, never advancing NPC steps, milestones, signals,
//! or credits, and never scheduling a follow-up. (The legacy path's
//! identity decode would also reject it: `payload >> 16` carries the
//! magic bits, far beyond `NPC_COUNT`.) The class targets a valid region
//! and rides the existing region-aware policy (a dormant target takes
//! the held path); the field selects the near/far lane explicitly.
//!
//! **The independent oracle:** the registry is built ONLY from
//! `schedule_event`'s successful `EventId` returns (rejections recorded
//! separately, never counted accepted) and is compared against the
//! spine's actual recorded execution identities — the ledger scan gives
//! per-identity execution counts (exactly-once and duplicate
//! detection), `collect_matching`/`held_view` give the queued / blocked
//! / held states — so `missing` (an accepted identity in NO state) is
//! the silent-drop signal. The whole-campaign conservation identity
//! `scheduled == executed + queued + blocked + held` is verified at
//! observation boundaries with the spine's own definitions; the living
//! NPC campaign's recurring obligations are never required to drain.
//!
//! **Determinism:** scenario construction is a pure function of fixed
//! inputs; the seq allocation is identical across identical runs, so
//! the baseline scenario double-runs and compares the accepted-EventId
//! sets, execution-identity sets, and verdicts. No wall-clock timing
//! determines logical behavior and no sleeps stand in for ordering —
//! every wait is a bounded logical-tick drain that fails clearly if the
//! required state is not reached.
//!
//! **Scope:** application/test-harness policy only — no engine-core,
//! engine-persist, renderer, shell, workflow, or S6-C1
//! writer/schema change; the accepted scheduling, causality, budget,
//! reservation, watermark, dormancy, and durability contracts are
//! exercised exactly as ratified (dequeue priority is NEVER confused
//! with execution order — causal precedence is verified through the
//! execution ledger).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use engine_core::{
    EventClass, EventId, EventSpec, Field, HistoryKind, ObligationLocation, PresenceSample,
    Schedule, Tick,
};
use engine_persist::{AuthorityMode, CanonicalMedium, RevisionRecord, T1Journal};
use engine_slice::orchestration::{OrchestratedSession, SubmitRefusal};
use engine_slice::persist_demo::{DEMO_PRESENCE, demo_scenario};
use engine_slice::sim::SimConfig;
use engine_slice::soak_evidence::{
    EvidenceCollector, EvidenceConfig, EvidenceRecord, TickSample, verify_package,
};
use engine_slice::store::{SessionStore, StoreLocation};

/// The burst cohort's payload tag — distinct from the routine-chain tag
/// `ROUTINE_MAGIC` (`0x5E47`); a burst payload can never decode as an
/// NPC routine chain.
pub const BURST_MAGIC: u64 = 0x5336;

/// Encodes one cohort member's payload: `magic(16) | cohort(16) |
/// index(32)`.
fn burst_payload(cohort: u16, index: u32) -> u64 {
    (BURST_MAGIC << 48) | (u64::from(cohort) << 32) | u64::from(index)
}

/// A unique campaign root (removed on drop — best-effort).
struct TestDir {
    root: PathBuf,
}

impl TestDir {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "s7-s6c2-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        TestDir { root }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The pre-founded campaign store (the worker discipline: the store
/// founded BEFORE the session opens — the same pre-founding every
/// orchestrated test uses).
fn found_store(dir: &Path) -> StoreLocation {
    let location = StoreLocation::Database(dir.join("campaign.db"));
    let mut store = SessionStore::open(&location).unwrap();
    if store.head().unwrap().is_none() {
        store
            .append(
                &RevisionRecord::founding(AuthorityMode::Standalone, "s6c2-burst", 0),
                &[],
                None,
            )
            .unwrap();
    }
    location
}

/// One cohort member's independent registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Registered {
    id: EventId,
    cohort: u16,
    index: u32,
}

/// One rejected scheduling attempt (recorded separately, never counted
/// as an accepted identity) — the scenario identity is part of the
/// inspectable record.
#[derive(Debug, Clone)]
struct Rejected {
    cohort: u16,
    index: u32,
    reason: String,
}

/// The per-cohort independent accounting verdict.
#[derive(Debug, Clone, PartialEq)]
struct CohortAccounting {
    expected: usize,
    executed: usize,
    duplicate_executions: Vec<EventId>,
    queued: usize,
    blocked: usize,
    held: usize,
    missing: Vec<EventId>,
    late_count: usize,
    max_lateness_ticks: u64,
}

impl CohortAccounting {
    /// No accepted identity is in NO state, none executed twice, and
    /// every identity is accounted in exactly the spine's states.
    fn fully_accounted(&self) -> bool {
        self.missing.is_empty()
            && self.duplicate_executions.is_empty()
            && self.executed + self.queued + self.blocked + self.held == self.expected
    }

    /// Fully accounted AND drained: the finite cohort's terminal state.
    fn complete(&self) -> bool {
        self.fully_accounted() && self.queued + self.blocked + self.held == 0
    }
}

/// The burst harness: one real orchestrated session (the worker-backed
/// crossing over the demo's own scenario construction) plus the
/// independent registries. The campaign directory lives as long as the
/// harness.
struct BurstHarness {
    session: OrchestratedSession,
    registered: Vec<Registered>,
    rejected: Vec<Rejected>,
    /// The TestDir's Drop cleans the campaign directory when the
    /// harness ends — kept alive, never read.
    #[allow(dead_code)]
    cleaner: TestDir,
}

impl BurstHarness {
    /// Opens a fresh orchestrated campaign with the demo's own scenario
    /// (the LOD-enabled 1000-NPC workload, the reserved spine, the
    /// director) under the given timing configuration.
    fn open(tag: &str, timing: SimConfig) -> Self {
        let cleaner = TestDir::new(tag);
        let location = found_store(&cleaner.root);
        let (spine, director) =
            demo_scenario(&timing, &[DEMO_PRESENCE]).expect("the scenario builds");
        let journal = T1Journal::open(cleaner.root.join("journal.t1")).unwrap();
        let session = OrchestratedSession::open(spine, director, &location, journal, "s6c2-burst")
            .expect("the session opens");
        BurstHarness {
            session,
            registered: Vec::new(),
            rejected: Vec::new(),
            cleaner,
        }
    }

    /// Schedules one cohort member through the accepted public boundary,
    /// recording the returned identity on success and the typed refusal
    /// separately on rejection. Returns the identity on acceptance.
    fn schedule_one(&mut self, spec: EventSpec, cohort: u16, index: u32) -> Option<EventId> {
        match self.session.schedule_event(spec) {
            Ok(id) => {
                self.registered.push(Registered { id, cohort, index });
                Some(id)
            }
            Err(error) => {
                self.rejected.push(Rejected {
                    cohort,
                    index,
                    reason: format!("{error:?}"),
                });
                None
            }
        }
    }

    /// Schedules a same-due cohort slice of `count` members into the
    /// given lane, targeting the given region class.
    fn schedule_slice(&mut self, cohort: u16, count: u32, due: Tick, field: Field, region: u16) {
        for index in 0..count {
            let spec = EventSpec::new(
                EventClass::new(region),
                Schedule::AtLogicalTime { due },
                burst_payload(cohort, index),
            )
            .in_field(field);
            self.schedule_one(spec, cohort, index);
        }
    }

    /// The cohort's registered identities.
    fn cohort_ids(&self, cohort: u16) -> HashSet<EventId> {
        self.registered
            .iter()
            .filter(|entry| entry.cohort == cohort)
            .map(|entry| entry.id)
            .collect()
    }

    /// The independent per-cohort accounting: the registry (built from
    /// `schedule_event`'s actual return values) against the spine's
    /// recorded execution identities and live obligation states. The
    /// ledger is the execution oracle — `is_executed` alone is only a
    /// membership check, so the scan counts per-identity executions
    /// (exactly-once and duplicate detection) and lateness directly.
    fn cohort_accounting(&self, cohort: u16) -> CohortAccounting {
        let ids = self.cohort_ids(cohort);
        let spine = self.session.spine();
        assert!(
            matches!(spine.history_kind(), HistoryKind::Full),
            "the bounded scenarios never compact — the full execution history \
             is required for identity-level verification"
        );
        let ledger = spine
            .try_ledger_full()
            .expect("the full history (asserted above)");
        let mut executed = HashSet::new();
        let mut duplicate_executions = Vec::new();
        let mut late_count = 0usize;
        let mut max_lateness_ticks = 0u64;
        for record in ledger {
            if !ids.contains(&record.id) {
                continue;
            }
            if !executed.insert(record.id) {
                duplicate_executions.push(record.id);
            }
            if record.late {
                late_count += 1;
                let lateness = record.executed_tick.as_u64() - record.intended_due.as_u64();
                max_lateness_ticks = max_lateness_ticks.max(lateness);
            }
        }
        // The live outstanding states, once, as identity sets: queued
        // and blocked through the queue scan, held through the held
        // view. `missing` — an accepted identity in NO state — is the
        // silent-drop signal.
        let mut outstanding = HashSet::new();
        for view in spine.collect_matching(|view| ids.contains(&view.id)) {
            match view.location {
                ObligationLocation::Queued => {
                    outstanding.insert(view.id);
                }
                ObligationLocation::Blocked => {
                    outstanding.insert(view.id);
                }
                ObligationLocation::Held => {
                    unreachable!("collect_matching never reports the held set")
                }
            }
        }
        let mut held = 0usize;
        for view in spine.held_view() {
            if ids.contains(&view.id) {
                held += 1;
                outstanding.insert(view.id);
            }
        }
        let queued = spine
            .collect_matching(|view| ids.contains(&view.id))
            .into_iter()
            .filter(|view| view.location == ObligationLocation::Queued)
            .count();
        let blocked = spine
            .collect_matching(|view| ids.contains(&view.id))
            .into_iter()
            .filter(|view| view.location == ObligationLocation::Blocked)
            .count();
        let mut missing = Vec::new();
        for id in &ids {
            if !executed.contains(id) && !outstanding.contains(id) {
                missing.push(*id);
            }
        }
        CohortAccounting {
            expected: ids.len(),
            executed: executed.len(),
            duplicate_executions,
            queued,
            blocked,
            held,
            missing,
            late_count,
            max_lateness_ticks,
        }
    }

    /// The whole-campaign conservation identity at an observation
    /// boundary, with the spine's own definitions (the living NPC
    /// campaign's recurring obligations are part of the accounted
    /// state — never required to drain).
    fn conservation_holds(&self) -> bool {
        let spine = self.session.spine();
        let stats = spine.stats();
        stats.scheduled
            == stats.executed
                + spine.pending_count() as u64
                + spine.blocked_count() as u64
                + spine.held_count() as u64
    }

    /// Runs `ticks` authoritative ticks under the given presence.
    fn run_ticks(&mut self, presence: &[PresenceSample], ticks: u64) {
        for _ in 0..ticks {
            self.session.tick(presence).expect("the tick runs");
        }
    }

    /// Drains the cohort to completion within a bounded logical-tick
    /// horizon, failing clearly if the required state is not reached.
    fn drain(
        &mut self,
        cohort: u16,
        presence: &[PresenceSample],
        max_ticks: u64,
    ) -> CohortAccounting {
        for _ in 0..max_ticks {
            let accounting = self.cohort_accounting(cohort);
            if accounting.complete() {
                return accounting;
            }
            self.session.tick(presence).expect("the tick runs");
        }
        let accounting = self.cohort_accounting(cohort);
        assert!(
            accounting.complete(),
            "the cohort did not drain within {max_ticks} ticks: {accounting:?} \
             (registered: {}, rejected: {})",
            self.registered.len(),
            self.rejected.len()
        );
        accounting
    }

    /// The sorted accepted-identity sequence set (the determinism
    /// comparison key).
    fn accepted_seqs(&self) -> Vec<u64> {
        let mut seqs: Vec<u64> = self.registered.iter().map(|entry| entry.id.seq).collect();
        seqs.sort_unstable();
        seqs
    }

    /// The sorted executed-identity sequence set for a cohort.
    fn executed_seqs(&self, cohort: u16) -> Vec<u64> {
        let ids = self.cohort_ids(cohort);
        let mut seqs: Vec<u64> = self
            .session
            .spine()
            .try_ledger_full()
            .expect("the full history")
            .iter()
            .filter(|record| ids.contains(&record.id))
            .map(|record| record.id.seq)
            .collect();
        seqs.sort_unstable();
        seqs
    }

    /// The world's total routine steps (the NPC-state witness).
    fn total_steps(&self) -> u64 {
        self.session.spine().world().total_steps()
    }

    /// The number of ledger executions that are NOT cohort members
    /// (the routine executions — the only steps source).
    fn routine_executions_total(&self) -> u64 {
        let cohort_ids: HashSet<EventId> = self.registered.iter().map(|entry| entry.id).collect();
        self.session
            .spine()
            .try_ledger_full()
            .expect("the full history")
            .iter()
            .filter(|record| !cohort_ids.contains(&record.id))
            .count() as u64
    }

    /// The number of ledger executions in a class that are NOT cohort
    /// members (the routine executions for that class).
    fn routine_executions_in_class(&self, class: u16) -> u64 {
        let cohort_ids: HashSet<EventId> = self.registered.iter().map(|entry| entry.id).collect();
        self.session
            .spine()
            .try_ledger_full()
            .expect("the full history")
            .iter()
            .filter(|record| record.class.0 == class && !cohort_ids.contains(&record.id))
            .count() as u64
    }
}

// -------------------------------------------------------------- B1

/// B1 — the baseline synchronized burst: the historically relevant
/// 200-event same-due cluster through the orchestrated boundary.
/// Verified: complete identity accounting mid-drain (the observation
/// boundary with an undrained cohort), exactly-once execution, honest
/// lateness, whole-campaign conservation throughout, and full drain.
/// The scenario double-runs: the accepted-identity sets,
/// execution-identity sets, and verdicts must be identical.
#[test]
fn b1_baseline_burst_conserves_every_identity() {
    let mut runs = Vec::new();
    for _ in 0..2 {
        let mut harness = BurstHarness::open("b1", SimConfig::default());
        harness.schedule_slice(1, 200, Tick(40), Field::Near, 0);
        assert_eq!(harness.rejected.len(), 0, "every attempt accepted");
        assert_eq!(harness.cohort_ids(1).len(), 200, "unique identities");

        // The observation boundary AT the due tick: whatever the
        // coincident NPC baseline demand, every identity is in
        // exactly one state — a nonempty outstanding set is honest
        // state, never a silent drop.
        harness.run_ticks(&[DEMO_PRESENCE], 40);
        let at_due = harness.cohort_accounting(1);
        assert!(at_due.fully_accounted(), "{at_due:?}");
        assert!(harness.conservation_holds(), "the campaign conserves");

        let drained = harness.drain(1, &[DEMO_PRESENCE], 600);
        assert!(drained.complete(), "{drained:?}");
        assert_eq!(drained.executed, 200);
        assert_eq!(drained.missing, Vec::new());
        assert_eq!(drained.duplicate_executions, Vec::new());
        // The honest lateness implication: identities outstanding at
        // the due tick necessarily execute late (their due has
        // passed); on-time full service is equally valid.
        let outstanding_at_due = at_due.queued + at_due.blocked + at_due.held;
        if outstanding_at_due > 0 {
            assert!(
                drained.late_count >= outstanding_at_due,
                "the deferred identities run late: {at_due:?} -> {drained:?}"
            );
        }
        assert!(harness.conservation_holds());
        let accepted = harness.accepted_seqs();
        let executed = harness.executed_seqs(1);
        let report = harness.session.close();
        assert!(report.failure.is_none(), "{:?}", report.failure);

        runs.push((accepted, executed, drained.complete(), drained.executed));
    }
    assert_eq!(runs[0].0, runs[1].0, "identical accepted identities");
    assert_eq!(runs[0].1, runs[1].1, "identical execution identities");
    assert_eq!(runs[0].2, runs[1].2, "identical completion verdicts");
    assert_eq!(runs[0].3, runs[1].3, "identical execution counts");
}

// -------------------------------------------------------------- B2

/// B2 — budget exhaustion: a burst exceeding the per-tick budget stays
/// explicitly outstanding, exhaustion is observed and recorded,
/// subsequent ticks make progress, and every accepted identity
/// eventually executes exactly once. Nothing disappears to shrink the
/// backlog.
#[test]
fn b2_budget_exhaustion_keeps_work_explicitly_outstanding() {
    let timing = SimConfig {
        budget: 100,
        near_reserve: Some(40),
        ..SimConfig::default()
    };
    let mut harness = BurstHarness::open("b2", timing);
    harness.schedule_slice(2, 150, Tick(20), Field::Near, 0);
    assert_eq!(harness.rejected.len(), 0);

    // Advance TO the burst's due tick: the burst plus the NPC
    // baseline exceeds the budget there — the exhaustion is observed,
    // the remainder outstanding.
    harness.run_ticks(&[DEMO_PRESENCE], 20);
    let stats = harness.session.spine().stats();
    assert!(
        stats.budget_exhausted_ticks > 0,
        "the exhaustion is recorded: {stats:?}"
    );
    let after_due = harness.cohort_accounting(2);
    assert!(after_due.fully_accounted(), "{after_due:?}");
    assert!(
        after_due.queued + after_due.blocked + after_due.held > 0,
        "explicitly outstanding: {after_due:?}"
    );

    // Subsequent ticks make progress (never a silent stall).
    harness.run_ticks(&[DEMO_PRESENCE], 1);
    let after_progress = harness.cohort_accounting(2);
    assert!(
        after_progress.executed > after_due.executed || after_progress.complete(),
        "progress continues: {after_due:?} -> {after_progress:?}"
    );
    assert!(harness.conservation_holds());

    let drained = harness.drain(2, &[DEMO_PRESENCE], 600);
    assert_eq!(drained.executed, 150, "{drained:?}");
    assert!(drained.complete());
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

// -------------------------------------------------------------- B3

/// B3 — near/far reservation: work in both lanes under the ACCEPTED
/// three-phase policy — the near reservation is a guaranteed
/// allocation (not a ceiling), the nonreserved far capacity is
/// respected, leftovers redistribute near-first, and neither lane
/// silently drops. Near backlogging only at total saturation is the
/// honest signal, never a defect by itself.
#[test]
fn b3_near_reservation_guarantees_the_allocation_under_far_flood() {
    let timing = SimConfig {
        budget: 60,
        near_reserve: Some(20),
        ..SimConfig::default()
    };
    let mut harness = BurstHarness::open("b3", timing);
    // Near work well beyond the reserve, and a far flood.
    harness.schedule_slice(3, 60, Tick(20), Field::Near, 0);
    harness.schedule_slice(4, 240, Tick(20), Field::Far, 2);
    assert_eq!(harness.rejected.len(), 0);
    assert_eq!(
        harness.cohort_ids(3).len() + harness.cohort_ids(4).len(),
        300
    );

    // Per-tick lane observations over the loaded window: the lane
    // totals never exceed the budget; the near reservation is a
    // guaranteed CAPACITY for due near work (not a promise that
    // not-yet-due near work executes), so the honest signal is the
    // near_backlogged flag — set only when due near work remained at
    // total exhaustion. Leftovers redistribute near-first; neither
    // lane silently drops (both cohorts drain below).
    for _ in 0..6 {
        harness.run_ticks(&[DEMO_PRESENCE], 1);
        let spine = harness.session.spine();
        let lane = spine.lane_report();
        assert!(
            lane.near_executed + lane.far_executed <= 60,
            "the budget is respected: {lane:?}"
        );
        if lane.near_backlogged {
            assert!(
                lane.near_executed + lane.far_executed >= 60,
                "near backlog implies total exhaustion — the accepted \
                 semantics, never a new fairness contract: {lane:?}"
            );
        }
        assert!(harness.conservation_holds());
    }

    // Both cohorts drain completely — neither lane drops.
    let near_drained = harness.drain(3, &[DEMO_PRESENCE], 900);
    let far_drained = harness.drain(4, &[DEMO_PRESENCE], 900);
    assert_eq!(near_drained.executed, 60, "{near_drained:?}");
    assert_eq!(far_drained.executed, 240, "{far_drained:?}");
    assert!(near_drained.complete() && far_drained.complete());
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

// -------------------------------------------------------------- B4

/// B4 — causal dependencies: a finite DAG (three chains and a
/// cross-class diamond). The dependents come due BEFORE their
/// prerequisites, so the blocking window is observable. Verified: no
/// dependent executes before its prerequisite (the LEDGER order —
/// actual execution, never dequeue priority), blocked work is
/// retained and released by execution, the cohort completes exactly
/// once, and an unknown dependency is explicitly rejected (recorded
/// as a rejection, never an accepted identity).
#[test]
fn b4_causal_dag_preserves_prerequisites_and_rejects_unknown_ids() {
    let mut harness = BurstHarness::open("b4", SimConfig::default());
    let mut edges: Vec<(EventId, EventId)> = Vec::new();

    // Three chains over class 0: each step depends on the previous,
    // and each step comes due EARLIER than its prerequisite — the
    // dependents block at their due until the prerequisite executes.
    for chain in 0..3u16 {
        let cohort = 10 + chain;
        let mut previous: Option<EventId> = None;
        for step in 0..4u32 {
            let due = Tick(43 - u64::from(step));
            let mut spec = EventSpec::new(
                EventClass::new(0),
                Schedule::AtLogicalTime { due },
                burst_payload(cohort, step),
            )
            .in_field(Field::Near);
            if let Some(prerequisite) = previous {
                spec = spec.with_dependencies(vec![prerequisite]);
            }
            let id = harness
                .schedule_one(spec, cohort, step)
                .expect("the chain member is accepted");
            if let Some(prerequisite) = previous {
                edges.push((prerequisite, id));
            }
            previous = Some(id);
        }
    }

    // A cross-class diamond: A(class 0, due 43) -> {B(class 0, due
    // 41), C(class 1, due 40)} -> D(class 1, due 42).
    let a = harness
        .schedule_one(
            EventSpec::new(
                EventClass::new(0),
                Schedule::AtLogicalTime { due: Tick(43) },
                burst_payload(20, 0),
            )
            .in_field(Field::Near),
            20,
            0,
        )
        .expect("A is accepted");
    let b = harness
        .schedule_one(
            EventSpec::new(
                EventClass::new(0),
                Schedule::AtLogicalTime { due: Tick(41) },
                burst_payload(20, 1),
            )
            .in_field(Field::Near)
            .with_dependencies(vec![a]),
            20,
            1,
        )
        .expect("B is accepted");
    let c = harness
        .schedule_one(
            EventSpec::new(
                EventClass::new(1),
                Schedule::AtLogicalTime { due: Tick(40) },
                burst_payload(20, 2),
            )
            .in_field(Field::Far)
            .with_dependencies(vec![a]),
            20,
            2,
        )
        .expect("C is accepted");
    let d = harness
        .schedule_one(
            EventSpec::new(
                EventClass::new(1),
                Schedule::AtLogicalTime { due: Tick(42) },
                burst_payload(20, 3),
            )
            .in_field(Field::Near)
            .with_dependencies(vec![b, c]),
            20,
            3,
        )
        .expect("D is accepted");
    edges.push((a, b));
    edges.push((a, c));
    edges.push((b, d));
    edges.push((c, d));

    // An unknown dependency is explicitly rejected — recorded as a
    // rejection, never counted as an accepted identity.
    let unknown = EventId { seq: 999_999 };
    harness.schedule_one(
        EventSpec::new(
            EventClass::new(0),
            Schedule::AtLogicalTime { due: Tick(44) },
            burst_payload(21, 0),
        )
        .with_dependencies(vec![unknown]),
        21,
        0,
    );
    assert_eq!(harness.rejected.len(), 1, "{:?}", harness.rejected);
    assert!(harness.rejected[0].reason.contains("UnknownDependency"));
    assert_eq!(
        (harness.rejected[0].cohort, harness.rejected[0].index),
        (21, 0),
        "the rejection's scenario identity is recorded"
    );
    assert!(harness.cohort_ids(21).is_empty());

    // The blocking window: at tick 40 the earliest dependents are due
    // while their prerequisites (due 43) have not executed — blocked
    // work is retained.
    harness.run_ticks(&[DEMO_PRESENCE], 41);
    let spine = harness.session.spine();
    assert!(
        spine.blocked_count() > 0,
        "the dependents block on their prerequisites"
    );
    assert!(harness.conservation_holds());

    // Drain everything; then verify the LEDGER order (actual
    // execution, never dequeue priority): every prerequisite executes
    // before its dependent.
    for cohort in [10u16, 11, 12, 20] {
        let drained = harness.drain(cohort, &[DEMO_PRESENCE], 600);
        assert!(drained.complete(), "{drained:?}");
    }
    let spine = harness.session.spine();
    let ledger = spine.try_ledger_full().expect("the full history");
    let position_of = |id: EventId| {
        ledger
            .iter()
            .position(|record| record.id == id)
            .unwrap_or_else(|| panic!("the identity {id} executed"))
    };
    for (prerequisite, dependent) in &edges {
        assert!(
            position_of(*prerequisite) < position_of(*dependent),
            "the prerequisite {prerequisite} executes before {dependent}"
        );
    }
    assert!(harness.conservation_holds());
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

// -------------------------------------------------------------- B5

/// B5 — dormant-region holds and reactivation: a cohort targeting a
/// dormant region takes the held path with its identity unchanged,
/// nothing executes while the region stays dormant, a valid
/// reactivation makes the held work eligible, and every identity
/// completes exactly once — with the NPC routine reconstruction and
/// consequence application riding the durable transition protocol
/// (never bypassed).
#[test]
fn b5_dormant_holds_preserve_identity_until_reactivation() {
    let mut harness = BurstHarness::open("b5", SimConfig::default());
    // Region 5 (-75, -75) is dormant under the demo's founding
    // presence; the region-aware policy routes its class to the held
    // path.
    let region_five_center = PresenceSample {
        position: (-75.0, -75.0),
        velocity: (0.0, 0.0),
    };
    harness.schedule_slice(5, 60, Tick(20), Field::Near, 5);
    assert_eq!(harness.rejected.len(), 0);

    // The work is HELD with its identity unchanged, and nothing
    // executes while the region remains dormant.
    let held = harness.cohort_accounting(5);
    assert_eq!(held.held, 60, "{held:?}");
    assert_eq!(held.executed, 0, "nothing runs while dormant: {held:?}");
    for entry in &harness.registered {
        let still_there = harness
            .session
            .spine()
            .held_view()
            .iter()
            .any(|view| view.id == entry.id);
        assert!(
            still_there,
            "the identity {} is unchanged in the held set",
            entry.id
        );
    }
    harness.run_ticks(&[DEMO_PRESENCE], 10);
    let after_dormant = harness.cohort_accounting(5);
    assert_eq!(after_dormant.executed, 0, "{after_dormant:?}");
    assert_eq!(after_dormant.held, 60);
    assert!(harness.conservation_holds());

    // The burst never advanced region 5's NPC state (the foreign
    // payload proof): the region's routine executions in the ledger
    // are the only steps source, and its chains are held too.
    assert_eq!(harness.routine_executions_in_class(5), 0);

    // A valid reactivation (the presence moves to the region's
    // center; the six-phase crossing runs) makes the held work
    // eligible — the durable protocol, never bypassed.
    harness.run_ticks(&[region_five_center], 1);
    let reactivating = harness.cohort_accounting(5);
    assert_eq!(
        reactivating.held + reactivating.queued + reactivating.blocked + reactivating.executed,
        60,
        "{reactivating:?}"
    );
    let drained = harness.drain(5, &[region_five_center], 600);
    assert_eq!(drained.executed, 60, "{drained:?}");
    assert!(drained.complete());
    // The region's routine chains resumed through the real
    // reconstruction (class-5 routine executions exist post-reactivation).
    assert!(
        harness.routine_executions_in_class(5) > 0,
        "the routine reconstruction resumed"
    );
    assert!(harness.conservation_holds());
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

// -------------------------------------------------------------- B6

/// B6 — sustained overload and recovery, with the S6-C1 evidence
/// package: a fixed logical-tick window whose injection exceeds the
/// service capacity, then the injection stops and the cohort drains.
/// Measured per tick: injected, executed, outstanding, queue backlog,
/// peak cohort backlog, lateness, and the drain length. The campaign
/// is never falsely declared recovered while genuinely overloaded.
#[test]
fn b6_sustained_overload_grows_then_drains_with_evidence() {
    let timing = SimConfig {
        budget: 60,
        near_reserve: Some(20),
        ..SimConfig::default()
    };
    let mut harness = BurstHarness::open("b6", timing);
    let evidence_root = TestDir::new("b6-evidence");
    let evidence_dir = evidence_root.root.join("evidence");
    let collector =
        EvidenceCollector::start(EvidenceConfig::new(&evidence_dir, "s6c2-b6-overload"))
            .expect("the evidence collector starts");

    const WINDOW: u64 = 30;
    const PER_TICK: u32 = 100;
    let mut executed_per_tick = Vec::new();
    let mut outstanding_per_tick = Vec::new();
    let mut queue_per_tick = Vec::new();

    for offset in 0..WINDOW {
        let due = Tick(1 + offset);
        harness.schedule_slice(6, PER_TICK, due, Field::Near, 0);
        let tick_start = Instant::now();
        harness
            .session
            .tick(&[DEMO_PRESENCE])
            .expect("the tick runs");
        let tick_ms = tick_start.elapsed().as_secs_f64() * 1000.0;
        let accounting = harness.cohort_accounting(6);
        executed_per_tick.push(accounting.executed as u64);
        outstanding_per_tick
            .push((accounting.queued + accounting.blocked + accounting.held) as u64);
        queue_per_tick.push(harness.session.spine().pending_count() as u64);
        // The S6-C1 instrumentation reuse: one tick record per tick
        // (the same TickSample the demo emits, harness-observed).
        let spine = harness.session.spine();
        let stats = spine.stats();
        let totals = spine.lane_totals();
        collector.produce(EvidenceRecord::Tick(TickSample {
            tick: spine.clock().as_u64(),
            wall_offset_ms: collector.wall_offset_ms(),
            tick_ms,
            ticks_due_total: spine.clock().as_u64(),
            outstanding_ticks: 0,
            executed: stats.executed,
            scheduled: stats.scheduled,
            late_arrivals: stats.late_arrivals,
            late_executions: stats.late_executions,
            blocked_transitions: stats.blocked_transitions,
            invalid_dropped: stats.invalid_dropped,
            budget_exhausted_ticks: stats.budget_exhausted_ticks,
            near_pending: spine.near_pending_count() as u32,
            far_pending: spine.far_pending_count() as u32,
            blocked: spine.blocked_count() as u32,
            held: spine.held_count() as u32,
            near_executed_total: totals.near_executed,
            far_executed_total: totals.far_executed,
            near_backlogged_ticks: totals.near_backlogged_ticks,
            far_backlogged_ticks: totals.far_backlogged_ticks,
            processed_through: spine.processed_through().map(|tick| tick.as_u64()),
            max_executed_due: spine.max_executed_due().map(|tick| tick.as_u64()),
            transitioned_regions: Vec::new(),
        }));
        assert!(harness.conservation_holds());
    }

    // The overload window: the outstanding cohort GROWS (the demand
    // exceeds capacity) and is never falsely declared recovered.
    assert!(
        outstanding_per_tick[WINDOW as usize - 1] > outstanding_per_tick[0],
        "the backlog grows under the overload: {outstanding_per_tick:?}"
    );
    assert!(
        outstanding_per_tick[WINDOW as usize - 1] > 0,
        "still overloaded at the window's end — never a false recovery"
    );
    let peak = *outstanding_per_tick.iter().max().unwrap();
    assert!(
        peak > PER_TICK as u64,
        "a genuine accumulated backlog: {peak}"
    );

    // The injection stops; the finite cohort drains completely.
    let drain_start = harness.session.spine().clock().as_u64();
    let drained = harness.drain(6, &[DEMO_PRESENCE], 900);
    let drain_ticks = harness.session.spine().clock().as_u64() - drain_start;
    assert_eq!(drained.executed, WINDOW as usize * PER_TICK as usize);
    assert!(drained.complete(), "{drained:?}");
    assert!(drained.max_lateness_ticks > 0, "the overload runs late");
    assert!(harness.conservation_holds());
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);

    // The evidence package verifies through the accepted independent
    // verifier.
    let outcome = collector.finalize();
    assert!(outcome.is_complete(), "{outcome:?}");
    let verification = verify_package(&evidence_dir).expect("the package verifies");
    assert!(
        verification.verified_complete(),
        "{:?}",
        verification.problems
    );

    // The measurements are recorded (reported as observations, not gates).
    eprintln!(
        "b6: injected/tick {} · cumulative executed at window end {} · \
         peak outstanding {peak} · max queue {} · drain ticks {drain_ticks} · \
         max lateness {} ticks · late {} of {}",
        PER_TICK,
        executed_per_tick[WINDOW as usize - 1],
        queue_per_tick.iter().max().unwrap(),
        drained.max_lateness_ticks,
        drained.late_count,
        drained.executed
    );
}

// -------------------------------------------------------------- B7

/// B7 — late arrivals and watermark correctness: accepted events with
/// due times behind the current clock (within the existing API
/// contract). Verified: lateness reported, identities accounted, the
/// safe processed-through watermark never overclaims (it stays below a
/// pending earlier-due event and remains conceptually separate from
/// max executed due), and conservation holds.
#[test]
fn b7_late_arrivals_report_lateness_without_watermark_overclaim() {
    let mut harness = BurstHarness::open("b7", SimConfig::default());
    harness.run_ticks(&[DEMO_PRESENCE], 50);
    let now = harness.session.spine().clock().as_u64();

    // A late arrival: due behind the current clock.
    let late_due = Tick(now.saturating_sub(5));
    harness.schedule_slice(7, 40, late_due, Field::Near, 0);
    assert_eq!(harness.rejected.len(), 0);
    let stats = harness.session.spine().stats();
    assert!(
        stats.late_arrivals > 0,
        "the lateness is reported: {stats:?}"
    );

    // While the late event is pending, the safe watermark cannot claim
    // completeness past its due (it may be None or at/below the due)
    // — and it is always <= max executed due.
    let spine = harness.session.spine();
    if let (Some(watermark), Some(max_executed)) =
        (spine.processed_through(), spine.max_executed_due())
    {
        assert!(
            watermark.as_u64() <= max_executed.as_u64(),
            "the watermark never exceeds executed work"
        );
    }
    if let Some(watermark) = spine.processed_through() {
        assert!(
            watermark.as_u64() <= late_due.as_u64(),
            "the watermark never overclaims past a pending earlier-due event: \
             {watermark} vs due {late_due}"
        );
    }
    assert!(harness.conservation_holds());

    // The identities remain accounted; the executions run late and are
    // recorded as such.
    let drained = harness.drain(7, &[DEMO_PRESENCE], 600);
    assert!(drained.complete(), "{drained:?}");
    assert_eq!(drained.executed, 40);
    assert!(
        drained.late_count > 0,
        "the lateness is first-class: {drained:?}"
    );
    assert!(drained.max_lateness_ticks >= 5, "{drained:?}");
    assert!(harness.conservation_holds());
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

// -------------------------------------------------------------- B8

/// B8 — persistence under burst load (bounded): the burst rides the
/// real application-level persistence — optional publications under
/// load (with the truthful Busy refusal at the saturation boundary),
/// a required canonical confirmation after a capture-bearing durable
/// transition, a graceful close, and an exact recovery. The
/// authoritative session stays the single source of state; required
/// commits are never falsely reported confirmed.
#[test]
fn b8_persistence_under_burst_load_closes_and_recovers_exactly() {
    let timing = SimConfig {
        budget: 80,
        near_reserve: Some(30),
        ..SimConfig::default()
    };
    let cleaner = TestDir::new("b8");
    let location = found_store(&cleaner.root);
    let (spine, director) = demo_scenario(&timing, &[DEMO_PRESENCE]).expect("the scenario builds");
    let journal_path = cleaner.root.join("journal.t1");
    let journal = T1Journal::open(&journal_path).unwrap();
    let mut session = OrchestratedSession::open(spine, director, &location, journal, "s6c2-b8")
        .expect("the session opens");

    // The burst cohort (the load) and the independent registry.
    let mut registered: Vec<EventId> = Vec::new();
    for offset in 0..10u64 {
        let due = Tick(20 + offset);
        for index in 0..20u32 {
            let spec = EventSpec::new(
                EventClass::new(0),
                Schedule::AtLogicalTime { due },
                burst_payload(8, index),
            )
            .in_field(Field::Near);
            let id = session
                .schedule_event(spec)
                .expect("the scheduling accepted");
            registered.push(id);
        }
        session.tick(&[DEMO_PRESENCE]).expect("the tick runs");
    }
    assert_eq!(registered.len(), 200);

    // Optional publications under load: collect the captures first,
    // then submit in a tight loop so the bounded queue genuinely
    // saturates — the typed Busy refusal is the truthful boundary,
    // then every admitted handle drains.
    session.tick(&[DEMO_PRESENCE]).expect("the tick runs");
    let captures: Vec<_> = (0..80)
        .map(|_| session.capture_snapshot().expect("the capture"))
        .collect();
    let mut admitted = 0usize;
    let mut busy = 0usize;
    let mut handles = Vec::new();
    for capture in captures {
        match session.submit_publication(capture) {
            Ok(handle) => {
                admitted += 1;
                handles.push(handle);
            }
            Err(SubmitRefusal::Busy) => busy += 1,
            Err(other) => panic!("the admission refusal: {other:?}"),
        }
    }
    assert!(
        busy > 0,
        "the saturation boundary is truthful: {admitted} admitted, {busy} busy"
    );
    for handle in handles {
        let _ = session.wait_publication(handle);
    }

    // A capture-bearing durable transition (the excursion pattern:
    // the antipodal presence drives region 0's six-phase
    // deactivation) followed by the REQUIRED canonical confirmation
    // under the residual load — the outcome reported truthfully.
    let far = PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    };
    session.tick(&[far]).expect("the far tick");
    session.tick(&[far]).expect("the far tick");
    session.tick(&[far]).expect("the far tick");
    let commit = session.commit_canonical().expect("the required commit");
    let commit_text = format!("{commit:?}");
    assert!(
        commit_text.contains("Confirmed") || commit_text.contains("NothingPending"),
        "the confirmation outcome is truthful: {commit_text}"
    );
    // The reactivation and the remaining load drain.
    session.tick(&[DEMO_PRESENCE]).expect("the near tick");
    for _ in 0..40 {
        session.tick(&[DEMO_PRESENCE]).expect("the tick runs");
    }

    // The independent accounting before the close: the cohort is fully
    // drained exactly once.
    let ids: HashSet<EventId> = registered.into_iter().collect();
    let ledger = session.spine().try_ledger_full().expect("the history");
    let executed: HashSet<EventId> = ledger
        .iter()
        .filter(|record| ids.contains(&record.id))
        .map(|record| record.id)
        .collect();
    assert_eq!(
        executed.len(),
        ids.len(),
        "the cohort completed exactly once"
    );
    let closing_clock = session.spine().clock().as_u64();
    let closing_fingerprint =
        engine_slice::region::stage4_state_fingerprint_hex(session.spine(), session.director());

    // The graceful close: independently recoverable.
    let close = session.close();
    assert!(close.failure.is_none(), "{:?}", close.failure);

    // The exact recovery: the restored clock and fingerprint, the
    // restored obligation state, and a continued tick.
    let mut recovered =
        OrchestratedSession::recover(&location, &journal_path, "s6c2-b8").expect("the recovery");
    assert_eq!(
        recovered.spine().clock().as_u64(),
        closing_clock,
        "the restored clock is exact"
    );
    assert_eq!(
        engine_slice::region::stage4_state_fingerprint_hex(recovered.spine(), recovered.director()),
        closing_fingerprint,
        "the restored fingerprint is exact"
    );
    assert_eq!(
        recovered.spine().stats().scheduled,
        recovered.spine().stats().executed
            + recovered.spine().pending_count() as u64
            + recovered.spine().blocked_count() as u64
            + recovered.spine().held_count() as u64,
        "the recovered obligations conserve"
    );
    recovered
        .tick(&[DEMO_PRESENCE])
        .expect("the recovered tick runs");
    let report = recovered.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

// ------------------------------------------- focused failure and boundary tests

/// A failed authority gate refuses scheduling — the typed refusal is
/// recorded as a rejection (never an accepted identity), the composed
/// gate refuses the tick, and the session's close still runs the
/// accepted finalization discipline (the poisoned worker's honest
/// failure, never a claimed success).
#[test]
fn authority_gate_refusals_are_rejections_and_the_close_stays_honest() {
    let mut harness = BurstHarness::open("gate", SimConfig::default());
    harness.session.poison();
    harness.schedule_slice(9, 5, Tick(20), Field::Near, 0);
    assert_eq!(harness.registered.len(), 0, "nothing accepted");
    assert_eq!(harness.rejected.len(), 5, "{:?}", harness.rejected);
    for rejection in &harness.rejected {
        assert!(rejection.reason.contains("Gate"), "{}", rejection.reason);
    }
    // The composed gate refuses the tick too — the error path is the
    // spine's own, never a silent stall.
    assert!(harness.session.tick(&[DEMO_PRESENCE]).is_err());
    // The close honestly reports the poisoned worker's failure.
    let report = harness.session.close();
    assert!(
        report.failure.is_some(),
        "the failed close is a reported failure — never a claimed success"
    );
}

/// An invalid dependency is explicitly rejected at schedule time and
/// never counted as an accepted identity (the focused companion to
/// B4's inline case).
#[test]
fn invalid_dependencies_are_explicitly_rejected() {
    let mut harness = BurstHarness::open("dep", SimConfig::default());
    let unknown = EventId { seq: 424_242 };
    let spec = EventSpec::new(
        EventClass::new(0),
        Schedule::AtLogicalTime { due: Tick(20) },
        burst_payload(30, 0),
    )
    .with_dependencies(vec![unknown]);
    harness.schedule_one(spec, 30, 0);
    assert_eq!(harness.registered.len(), 0);
    assert_eq!(harness.rejected.len(), 1);
    assert!(
        harness.rejected[0].reason.contains("UnknownDependency"),
        "{}",
        harness.rejected[0].reason
    );
    assert_eq!(
        (harness.rejected[0].cohort, harness.rejected[0].index),
        (30, 0),
        "the rejection's scenario identity is recorded"
    );
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

/// Repeated observations of an already-executed identity are stable
/// and side-effect-free: `is_executed` is a read-only membership
/// check, and the campaign's counters do not move across repeated
/// observations.
#[test]
fn repeated_identity_observations_are_stable() {
    let mut harness = BurstHarness::open("observe", SimConfig::default());
    harness.schedule_slice(31, 10, Tick(20), Field::Near, 0);
    let identity = harness.registered[0].id;
    // Advance past the due tick so the identity has executed.
    harness.run_ticks(&[DEMO_PRESENCE], 30);
    assert!(harness.session.spine().is_executed(identity));
    let stats_before = harness.session.spine().stats();
    for _ in 0..10 {
        assert!(harness.session.spine().is_executed(identity));
    }
    let stats_after = harness.session.spine().stats();
    assert_eq!(
        (stats_before.executed, stats_before.scheduled),
        (stats_after.executed, stats_after.scheduled),
        "the observation is side-effect-free"
    );
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

/// Foreign burst payloads never advance NPC routine state: over a
/// burst window in an ACTIVE region, the world's total steps advance
/// by EXACTLY the routine executions recorded in the ledger (the
/// cohort's executions excluded) — the burst contributes steps
/// nowhere, and no milestone machinery fires for it.
#[test]
fn foreign_payloads_never_advance_npc_routine_state() {
    let mut harness = BurstHarness::open("foreign", SimConfig::default());
    let steps_before = harness.total_steps();
    let routine_before = harness.routine_executions_total();
    // A burst into the ACTIVE region 0 (class 0) — 120 events over
    // several ticks.
    for offset in 0..6u64 {
        harness.schedule_slice(32, 20, Tick(20 + offset), Field::Near, 0);
        harness.run_ticks(&[DEMO_PRESENCE], 1);
    }
    let drained = harness.drain(32, &[DEMO_PRESENCE], 600);
    assert_eq!(drained.executed, 120, "{drained:?}");
    let steps_after = harness.total_steps();
    let routine_after = harness.routine_executions_total();
    assert_eq!(
        steps_after - steps_before,
        routine_after - routine_before,
        "the world's steps advance ONLY per routine execution — the burst \
         contributed nothing ({} new steps, {} new routine executions)",
        steps_after - steps_before,
        routine_after - routine_before
    );
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

/// A non-dormant target schedules into the live queue (never the held
/// path) — the region-aware policy's other half.
#[test]
fn active_region_targets_schedule_into_the_live_queue() {
    let mut harness = BurstHarness::open("active", SimConfig::default());
    harness.schedule_slice(33, 10, Tick(100), Field::Far, 0);
    let accounting = harness.cohort_accounting(33);
    assert_eq!(accounting.queued, 10, "{accounting:?}");
    assert_eq!(accounting.held, 0, "{accounting:?}");
    assert!(harness.conservation_holds());
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}

/// The observation boundary with an undrained cohort distinguishes a
/// valid outstanding state from a silent drop: every accepted identity
/// is in exactly one state, none missing, none duplicated — and the
/// conservation identity holds at the boundary (the focused companion
/// to the B1/B2 inline checks).
#[test]
fn observation_boundaries_account_undrained_cohorts_exactly() {
    let timing = SimConfig {
        budget: 50,
        near_reserve: Some(20),
        ..SimConfig::default()
    };
    let mut harness = BurstHarness::open("boundary", timing);
    harness.schedule_slice(34, 300, Tick(20), Field::Near, 0);
    for boundary in 0..5u64 {
        harness.run_ticks(&[DEMO_PRESENCE], 1);
        let accounting = harness.cohort_accounting(34);
        assert!(
            accounting.fully_accounted(),
            "boundary {boundary}: {accounting:?}"
        );
        assert!(
            accounting.queued + accounting.blocked + accounting.held > 0,
            "boundary {boundary}: the cohort is honestly undrained"
        );
        assert!(harness.conservation_holds());
    }
    let drained = harness.drain(34, &[DEMO_PRESENCE], 900);
    assert!(drained.complete(), "{drained:?}");
    let report = harness.session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
}
