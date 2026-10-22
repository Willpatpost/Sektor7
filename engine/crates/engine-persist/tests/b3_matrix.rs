//! The Stage 5 Batch 3 failure-first verification matrix — the ten
//! §5.4.1 acceptance scenarios driven at the GENERIC full-stack level
//! (the durable T1 journal + the real `LocalFileMedium` + the
//! `CommitPipeline` + the `EncounterJournal` + the recovery
//! reconciliation composed against real files), with the identity and
//! accounting discipline asserted after every simulated recovery.
//!
//! **Traceability (explicit, per the GO):** every scenario below is
//! *implemented-B3 generic* — deterministic generic fixtures standing
//! in for the application state. The architecture's ideal form
//! exercises the REAL `NpcWorld` award rule (`MILESTONE_PERIOD = 16`,
//! steps-derived thresholds); that integration is **Batch 4** and is
//! never claimed here: the "step-16 crossing" of scenarios 7–8 is
//! represented by occurrence 1 under the uniform per-occurrence unit,
//! and world+character atomicity is asserted as the envelope's
//! whole-or-nothing application plus cross-lane coherence. The
//! journal's own durability matrix (torn tails, digest/count
//! corruption, ambiguity, cancellation), the pipeline's codec/stale/
//! unknown/medium-failure windows, and the encounter's AwardState
//! regressions are covered in-module (journal.rs 21, pipeline.rs 15,
//! encounter.rs 13, recovery.rs 16, envelope.rs 25, consequence.rs
//! 31) and by the real child-process suites (`journal_process.rs`,
//! `process.rs`, `b3_process.rs`); THIS file drives the crash-window
//! COMPOSITIONS — what a fresh process observes and re-derives after
//! death at each window. *Awaiting-B4:* the real award rule, the
//! Spine/RegionDirector reconstruction (Contract B's exact
//! transitions), and the snapshot-published anchor (here the anchor
//! is the caller-constructed generic form). *Blocked:* nothing in the
//! generic machinery.
//!
//! **The simulated crash is honest:** dropping every instance and
//! reopening from disk leaves exactly what a process death leaves
//! (the OS reclaims the locks; nothing durable claims representation
//! — the W4 property). Real child-process deaths at the stack level
//! live in `tests/b3_process.rs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use engine_persist::{
    AppendOutcome, AuthorityMode, AwardCheckpoint, AwardState, CanonicalMedium, ClassifiedSpan,
    CommitOutcome, CommitPipeline, ConsequenceEnvelope, ConsequenceId, CounterDelta, CounterValues,
    Coverage, DurabilityClass, EncounterJournal, FaultAction, FaultControl, FaultPoint,
    FrontierEntry, KeyPhase, LocalFileMedium, OccurrenceSpan, RecoveryAnchor, RecoveryOutcome,
    RevisionRecord, SignalLane, SourceBaseline, SourcePosition, T1Journal, TransactionDurability,
    TransactionInput, TransitionClassification, reconcile,
};

// ---------------------------------------------------------------- setup

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("s7-b3-matrix-{}-{label}-{seq}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test root");
        TestDir(path)
    }

    fn store(&self) -> PathBuf {
        self.0.join("store")
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.t1")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Idempotently founds the campaign store (the application's setup
/// act, never a per-commit decision). The founding instance is
/// dropped before the pipeline opens its own medium.
fn ensure_founded(dir: &TestDir) {
    let mut medium = LocalFileMedium::open(dir.store()).unwrap();
    if medium.head().unwrap().is_some() {
        return;
    }
    let founding = RevisionRecord::founding(AuthorityMode::Standalone, "matrix-campaign", 0);
    match medium.append(&founding, &[], None).unwrap() {
        AppendOutcome::Accepted { .. } => {}
        other => panic!("the founding must accept: {other:?}"),
    }
}

fn fresh_pipeline(dir: &TestDir) -> CommitPipeline<LocalFileMedium> {
    ensure_founded(dir);
    let journal = T1Journal::open(dir.journal()).unwrap();
    CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal).unwrap()
}

fn position(tick: u64) -> SourcePosition {
    SourcePosition { head: None, tick }
}

fn frontier(episode: u64) -> Vec<FrontierEntry> {
    vec![FrontierEntry { region: 1, episode }]
}

/// `milestones` total with `lane_value` units sitting in `lane` — the
/// uniform per-occurrence unit's counter shape.
fn counters(milestones: u64, lane: SignalLane, lane_value: u64) -> CounterValues {
    let mut values = CounterValues {
        milestones,
        pending_signals: 0,
        captured: 0,
        credits: 0,
    };
    match lane {
        SignalLane::Pending => values.pending_signals = lane_value,
        SignalLane::Captured => values.captured = lane_value,
        SignalLane::Credits => values.credits = lane_value,
    }
    values
}

/// An award state whose durable lane covers exactly the given
/// occurrences (the anchor's materialized coverage).
fn durable_award(occurrences: &[u64]) -> AwardState {
    let mut award = AwardState::new();
    for occurrence in occurrences {
        award.award_durable(*occurrence);
    }
    award
}

fn empty_baseline() -> SourceBaseline {
    SourceBaseline {
        counters: CounterValues::default(),
        award: AwardCheckpoint {
            durable: Coverage::new(),
            provisional: Coverage::new(),
        },
    }
}

fn capture_envelope(
    id: ConsequenceId,
    span: (u64, u64),
    lane: SignalLane,
    tick: u64,
) -> ConsequenceEnvelope {
    ConsequenceEnvelope::capture(
        id,
        vec![ClassifiedSpan {
            span: OccurrenceSpan::new(span.0, span.1).unwrap(),
            class: DurabilityClass::DurableEligible,
        }],
        lane,
        CounterValues::default(),
        empty_baseline(),
        position(tick),
    )
    .unwrap()
}

/// A capture prepared from a non-empty source state: the baseline
/// carries the pre-span coverage and counters observed at preparation
/// (§5.4.1: `lo` is the last already-covered occurrence — the
/// real-world envelope records the prior capture's watermark, and the
/// Contract B comparable-state check attributes the anchor's
/// milestones to exactly baseline + newly-materialized).
fn capture_envelope_with_baseline(
    id: ConsequenceId,
    span: (u64, u64),
    lane: SignalLane,
    tick: u64,
    baseline_counters: CounterValues,
    baseline_durable: &[u64],
) -> ConsequenceEnvelope {
    let mut prior_award = AwardState::new();
    for occurrence in baseline_durable {
        prior_award.award_durable(*occurrence);
    }
    ConsequenceEnvelope::capture(
        id,
        vec![ClassifiedSpan {
            span: OccurrenceSpan::new(span.0, span.1).unwrap(),
            class: DurabilityClass::DurableEligible,
        }],
        lane,
        baseline_counters,
        SourceBaseline {
            counters: baseline_counters,
            award: AwardCheckpoint {
                durable: prior_award.durable,
                provisional: Coverage::new(),
            },
        },
        position(tick),
    )
    .unwrap()
}

/// The mixed-class aggregate: one identity, class-homogeneous
/// sub-spans (the legitimate partition — different-class adjacency).
fn mixed_class_envelope(
    id: ConsequenceId,
    durable: (u64, u64),
    provisional: (u64, u64),
    lane: SignalLane,
    tick: u64,
) -> ConsequenceEnvelope {
    ConsequenceEnvelope::capture(
        id,
        vec![
            ClassifiedSpan {
                span: OccurrenceSpan::new(durable.0, durable.1).unwrap(),
                class: DurabilityClass::DurableEligible,
            },
            ClassifiedSpan {
                span: OccurrenceSpan::new(provisional.0, provisional.1).unwrap(),
                class: DurabilityClass::EncounterProvisional,
            },
        ],
        lane,
        CounterValues::default(),
        empty_baseline(),
        position(tick),
    )
    .unwrap()
}

/// A span-less lane movement (an activation commit): net-zero lane
/// movement, applied whole-or-not by identity.
fn movement_envelope(
    id: ConsequenceId,
    captured: i64,
    credits: i64,
    prior: CounterValues,
    tick: u64,
) -> ConsequenceEnvelope {
    ConsequenceEnvelope::new(id)
        .with_delta(CounterDelta {
            milestones: 0,
            pending_signals: 0,
            captured,
            credits,
        })
        .with_expected_prior(prior)
        .with_baseline(SourceBaseline {
            counters: prior,
            award: AwardCheckpoint {
                durable: Coverage::new(),
                provisional: Coverage::new(),
            },
        })
        .with_position(position(tick))
}

fn tx(envelopes: Vec<ConsequenceEnvelope>, tick: u64, episode: u64) -> TransactionInput {
    TransactionInput {
        envelopes,
        position: position(tick),
        classification: DurabilityClass::DurableEligible,
        dependencies: Vec::new(),
        frontier: frontier(episode),
        plan: format!("matrix-plan-{episode}").into_bytes(),
    }
}

fn anchor_at(tick: u64) -> RecoveryAnchor {
    RecoveryAnchor::new(position(tick))
}

/// The identity and accounting discipline, asserted after EVERY
/// simulated recovery: conservation per participant, cross-lane
/// disjoint coverage, and one application entry per
/// (identity, transaction) pair — never a duplicated pair.
fn assert_recovery_invariants(outcome: &RecoveryOutcome) {
    for (id, participant) in &outcome.recovered {
        assert!(
            participant.counters.conservation_holds(),
            "conservation must hold for {id} after every recovery: {:?}",
            participant.counters
        );
        assert!(
            participant.award.validate().is_ok(),
            "the recovered award lanes stay cross-lane disjoint for {id}"
        );
    }
    let mut applied_pairs = BTreeSet::new();
    for effect in &outcome.report.applied {
        assert!(
            applied_pairs.insert((effect.id, effect.transaction)),
            "one application entry per (identity, transaction) — never duplicated"
        );
    }
}

/// The empty confirmed set (recovery's tier-1 input).
fn no_chain() -> BTreeSet<ConsequenceId> {
    BTreeSet::new()
}

// ------------------------------------------- the ten §5.4.1 scenarios

/// §5.4.1 (1) — the v1.7 durable-marker crash (marker recorded,
/// snapshot not) cannot occur: the marker no longer exists by
/// construction. The equivalent crash — after live application,
/// before snapshot publication (W4) — re-replays from the anchor with
/// no loss and no duplication.
#[test]
fn marker_window_eliminated_w4() {
    let dir = TestDir::new("w4");
    let id = ConsequenceId::new(1, 1, 1);
    let tx_id;
    {
        let mut pipeline = fresh_pipeline(&dir);
        pipeline
            .stage(capture_envelope(id, (0, 2), SignalLane::Captured, 110))
            .unwrap();
        let receipt = pipeline
            .capture(
                position(110),
                DurabilityClass::DurableEligible,
                Vec::new(),
                frontier(1),
                b"matrix-plan-1".to_vec(),
            )
            .unwrap();
        assert!(!receipt.reconciled);
        tx_id = receipt.journal.id;
        // The live application (in-memory only — nothing durable
        // claims representation; the removed v1.7 marker is the point).
        pipeline.mark_applied(&[id]).unwrap();
        assert_eq!(
            pipeline.status(id).unwrap().phase,
            KeyPhase::AppliedToLivePending
        );
        drop(pipeline);
    }
    // The fresh process: committed and actionable, NEVER a fabricated
    // acknowledgment across the boundary.
    {
        let journal = T1Journal::open(dir.journal()).unwrap();
        assert_eq!(journal.transactions().len(), 1);
        assert_eq!(
            journal.transaction_status(tx_id),
            Some(TransactionDurability::Committed)
        );
        assert!(!journal.acknowledged_this_session(tx_id));
        assert!(journal.is_actionable(tx_id));
    }
    // The W4 re-replay: the anchor predates everything; the envelope
    // applies exactly once — no loss, no duplication.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let anchor = anchor_at(100);
    let outcome = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    let participant = outcome.recovered.get(&id).unwrap();
    assert_eq!(
        participant.counters,
        counters(2, SignalLane::Captured, 2),
        "exactly once — the application that died with the process leaves no trace"
    );
    assert!(participant.award.durable.covers(1) && participant.award.durable.covers(2));
    // The re-drive under the original identity confirms exactly-once.
    let mut pipeline = fresh_pipeline(&dir);
    assert_eq!(pipeline.status(id).unwrap().phase, KeyPhase::JournaledT1);
    let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
        panic!("expected confirmation");
    };
    assert_eq!(confirmation.keys, vec![id]);
    assert_eq!(
        pipeline.status(id).unwrap().phase,
        KeyPhase::InConfirmedChain
    );
    assert_eq!(pipeline.confirmed_coverage(id).unwrap().len(), 1);
    // Post-confirmation: the chain tier suppresses any re-recovery.
    let confirmed: BTreeSet<ConsequenceId> = pipeline.confirmed_keys().into_iter().collect();
    let outcome = reconcile(&anchor, &scan, &confirmed).unwrap();
    assert!(outcome.report.applied.is_empty());
    assert!(outcome.report.chain_suppressed.contains(&id));
    drop(pipeline);
    let mut check = LocalFileMedium::open(dir.store()).unwrap();
    assert_eq!(check.validate().unwrap().records, 2);
}

/// §5.4.1 (2) — crash after state application but before any
/// acknowledgment: nothing durable claims representation; the
/// re-derivation reproduces the same state (W3's partial in-memory
/// application dies with the process; W4's completed application
/// leaves no marker — both re-derive identically).
#[test]
fn application_before_acknowledgment_w3_w4() {
    let dir = TestDir::new("w3-w4");
    let id = ConsequenceId::new(1, 1, 1);
    {
        let mut pipeline = fresh_pipeline(&dir);
        pipeline
            .stage(capture_envelope(id, (0, 3), SignalLane::Pending, 110))
            .unwrap();
        pipeline
            .capture(
                position(110),
                DurabilityClass::DurableEligible,
                Vec::new(),
                frontier(1),
                b"matrix-plan-1".to_vec(),
            )
            .unwrap();
        pipeline.mark_applied(&[id]).unwrap();
        drop(pipeline);
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let anchor = anchor_at(100);
    let first = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&first);
    // A second crash before publication re-derives the identical state.
    let second = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_eq!(first, second, "the pure function — identical re-derivation");
    let participant = first.recovered.get(&id).unwrap();
    assert_eq!(participant.counters, counters(3, SignalLane::Pending, 3));
    assert!(participant.counters.conservation_holds());
}

/// §5.4.1 (3) — recovery from an older anchor with the journal
/// containing envelopes applied in earlier recovery runs but never
/// published: the prior applications do not suppress (no durable
/// marker); the envelopes re-replay idempotently to the same result —
/// and the published branch (a newer anchor exists) suppresses via
/// its coverage.
#[test]
fn older_anchor_multiple_prior_applies() {
    let dir = TestDir::new("older-anchor");
    let a = ConsequenceId::new(1, 1, 1);
    let b = ConsequenceId::new(1, 2, 1);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(a, (0, 2), SignalLane::Captured, 110)],
                110,
                1,
            ))
            .unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(b, (2, 4), SignalLane::Captured, 115)],
                115,
                2,
            ))
            .unwrap();
    }
    // Two earlier recovery runs applied these envelopes and crashed
    // before any publication — the prior applications never suppress.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let anchor = anchor_at(100);
    let run_one = reconcile(&anchor, &scan, &no_chain()).unwrap();
    let run_two = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_eq!(run_one, run_two, "prior applies are not durable markers");
    assert_recovery_invariants(&run_one);
    assert_eq!(
        run_one.recovered.get(&a).unwrap().counters,
        counters(2, SignalLane::Captured, 2)
    );
    assert_eq!(
        run_one.recovered.get(&b).unwrap().counters,
        counters(2, SignalLane::Captured, 2)
    );
    // The published branch: a newer anchor carries the recovered
    // coverage — the materialized tier suppresses without duplication.
    let published = anchor_at(120)
        .with_identity(
            a,
            counters(2, SignalLane::Captured, 2),
            durable_award(&[1, 2]),
        )
        .with_identity(
            b,
            counters(2, SignalLane::Captured, 2),
            durable_award(&[3, 4]),
        );
    let after = reconcile(&published, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&after);
    assert!(after.report.applied.is_empty());
    assert!(after.report.materialized.contains(&(a, 2)));
    assert!(after.report.materialized.contains(&(b, 2)));
    assert_eq!(
        after.recovered.get(&a).unwrap().counters,
        counters(2, SignalLane::Captured, 2),
        "the published state passes through unchanged"
    );
}

/// §5.4.1 (4) — an anchor containing occurrences 3–5 of aggregate X's
/// span (3,7]: only 6–7 apply; X is neither wholly applied nor wholly
/// absent.
#[test]
fn aggregate_partially_present_in_anchor() {
    let dir = TestDir::new("partial");
    let x = ConsequenceId::new(2, 1, 5);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        // The aggregate's `lo` = 3 is the previous capture's
        // watermark: the baseline carries occurrence 3 (one unit)
        // observed at preparation.
        journal
            .append_transaction(&tx(
                vec![capture_envelope_with_baseline(
                    x,
                    (3, 7),
                    SignalLane::Captured,
                    110,
                    counters(1, SignalLane::Captured, 1),
                    &[3],
                )],
                110,
                1,
            ))
            .unwrap();
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    // The anchor materializes occurrences 3–5: the baseline's one
    // unit plus the two newly-materialized occurrences — the
    // coherent at-or-after shape.
    let anchor = anchor_at(120).with_identity(
        x,
        counters(3, SignalLane::Captured, 3),
        durable_award(&[3, 4, 5]),
    );
    let outcome = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    assert_eq!(
        outcome.report.transitions[0].1,
        TransitionClassification::PartialState
    );
    let participant = outcome.recovered.get(&x).unwrap();
    assert_eq!(
        participant.counters,
        counters(5, SignalLane::Captured, 5),
        "exactly the missing occurrences 6–7 apply"
    );
    assert!(participant.award.durable.covers(6) && participant.award.durable.covers(7));
    let effect = &outcome.report.applied[0];
    assert_eq!(effect.applied.len(), 1);
    assert_eq!(
        (effect.applied[0].span.lo, effect.applied[0].span.hi),
        (5, 7),
        "the applied span is exactly the missing suffix"
    );
    assert_eq!(effect.delta.milestones, 2);
    assert_eq!(effect.delta.captured, 2);
}

/// §5.4.1 (5) — multiple aggregate envelopes with adjacent `(0,5]` +
/// `(5,9]` and overlapping `(0,5]` + `(3,7]` spans: union coverage
/// composes exactly; uniform per-occurrence deltas; no double-apply.
#[test]
fn adjacent_and_overlapping_spans() {
    // Adjacent under one identity: the second aggregate's occurrences
    // compose onto the first's coverage.
    let dir = TestDir::new("adjacent");
    let id = ConsequenceId::new(3, 1, 1);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(id, (0, 5), SignalLane::Captured, 110)],
                110,
                1,
            ))
            .unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(id, (5, 9), SignalLane::Captured, 115)],
                115,
                2,
            ))
            .unwrap();
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let outcome = reconcile(&anchor_at(100), &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    let participant = outcome.recovered.get(&id).unwrap();
    assert_eq!(
        participant.counters,
        counters(9, SignalLane::Captured, 9),
        "both aggregates apply — adjacent spans never double-count"
    );
    assert_eq!(
        participant.award.durable.spans().len(),
        1,
        "the coverage composes to one span"
    );
    assert!(participant.award.durable.covers(9));

    // Overlapping under one identity: the overlap 4–5 applies exactly
    // once; only 6–7 of the second aggregate are missing.
    let dir = TestDir::new("overlap");
    let id = ConsequenceId::new(3, 1, 1);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(id, (0, 5), SignalLane::Captured, 110)],
                110,
                1,
            ))
            .unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(id, (3, 7), SignalLane::Captured, 115)],
                115,
                2,
            ))
            .unwrap();
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let outcome = reconcile(&anchor_at(100), &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    let participant = outcome.recovered.get(&id).unwrap();
    assert_eq!(
        participant.counters,
        counters(7, SignalLane::Captured, 7),
        "the overlapping occurrences 4–5 never double-apply"
    );
    assert_eq!(participant.award.durable.spans().len(), 1);
    assert!(participant.award.durable.covers(7));
}

/// §5.4.1 (6) — exact lower-bound and upper-bound occurrence
/// coverage: an anchor at `A = lo` applies the full span; an anchor
/// at `A = hi` applies none; the `(lo, hi]` convention verified at
/// both bounds.
#[test]
fn exact_bound_coverage() {
    let dir = TestDir::new("bounds");
    let x = ConsequenceId::new(4, 1, 2);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        // `lo` = 3: the baseline carries the prior capture's
        // watermark (occurrences 1–3, three units).
        journal
            .append_transaction(&tx(
                vec![capture_envelope_with_baseline(
                    x,
                    (3, 7),
                    SignalLane::Captured,
                    110,
                    counters(3, SignalLane::Captured, 3),
                    &[1, 2, 3],
                )],
                110,
                1,
            ))
            .unwrap();
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    // A = lo: the anchor covers through occurrence 3 — the FULL span
    // (4–7) applies.
    let at_lo = anchor_at(120).with_identity(
        x,
        counters(3, SignalLane::Captured, 3),
        durable_award(&[1, 2, 3]),
    );
    let outcome = reconcile(&at_lo, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    assert_eq!(
        outcome.report.transitions[0].1,
        TransitionClassification::PreState
    );
    assert_eq!(
        outcome.recovered.get(&x).unwrap().counters,
        counters(7, SignalLane::Captured, 7)
    );
    // A = hi: the anchor covers through occurrence 7 — nothing
    // applies, no duplicate.
    let at_hi = anchor_at(120).with_identity(
        x,
        counters(7, SignalLane::Captured, 7),
        durable_award(&[1, 2, 3, 4, 5, 6, 7]),
    );
    let outcome = reconcile(&at_hi, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    assert_eq!(
        outcome.report.transitions[0].1,
        TransitionClassification::PostState
    );
    assert_eq!(
        outcome.recovered.get(&x).unwrap().counters,
        counters(7, SignalLane::Captured, 7),
        "nothing applies at the upper bound"
    );
    assert!(outcome.report.applied.is_empty());
}

/// §5.4.1 (7) — the review's example: the encounter provisionally
/// awards (occurrence 1 — the generic stand-in for the step-16
/// milestone crossing), fails and rolls back (the occurrence becomes
/// earnable again); the reattempt's award fires legitimately, and the
/// committed boundary transaction recovers exactly once. The REAL
/// `NpcWorld` award rule (`MILESTONE_PERIOD = 16`) is B4 — never
/// claimed here.
#[test]
fn provisional_award_failed_encounter_reattempt() {
    let dir = TestDir::new("provisional-reattempt");
    let id = ConsequenceId::new(5, 1, 1);
    // The failing attempt: a provisional award, rolled back with the
    // transaction.
    let mut failed = EncounterJournal::new(SignalLane::Captured);
    failed
        .begin_participant(id, AwardState::new(), CounterValues::default())
        .unwrap();
    assert!(
        failed
            .record_effect(id, 1, DurabilityClass::EncounterProvisional)
            .unwrap()
    );
    failed.rollback().unwrap();
    let (award, restored) = failed.participant_state(id).unwrap();
    assert!(
        award.provisional.is_empty() && award.durable.is_empty(),
        "the rolled-back provisional award restores eligibility"
    );
    assert_eq!(*restored, CounterValues::default());
    // The reattempt: the legitimate award fires.
    let mut pipeline = fresh_pipeline(&dir);
    let mut reattempt = EncounterJournal::new(SignalLane::Captured);
    reattempt
        .begin_participant(id, AwardState::new(), CounterValues::default())
        .unwrap();
    assert!(
        reattempt
            .record_effect(id, 1, DurabilityClass::EncounterProvisional)
            .unwrap(),
        "the rolled-back occurrence is legitimately earnable again"
    );
    let receipt = reattempt
        .commit(
            &mut pipeline,
            position(120),
            Vec::new(),
            frontier(1),
            b"encounter-reattempt".to_vec(),
        )
        .unwrap()
        .expect("effects were accrued");
    assert!(!receipt.reconciled);
    drop(pipeline);
    // Crash before canonical confirmation: recovery from the
    // pre-encounter anchor applies the committed boundary exactly
    // once — never the failed attempt's phantom.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let anchor = anchor_at(100);
    let outcome = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    let participant = outcome.recovered.get(&id).unwrap();
    assert_eq!(
        participant.counters,
        counters(1, SignalLane::Captured, 1),
        "the committed reattempt applies exactly once"
    );
    assert!(participant.award.durable.covers(1));
    // The canonical confirmation is the pipeline's separate act.
    let mut pipeline = fresh_pipeline(&dir);
    let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
        panic!("expected confirmation");
    };
    assert_eq!(confirmation.keys, vec![id]);
}

/// §5.4.1 (8) — a durably-retained `DurableEligible` award followed
/// by a failed encounter: the durable contribution survives the
/// rollback and is NOT re-awarded on the reattempt.
#[test]
fn durable_award_failed_encounter_no_duplicate() {
    let dir = TestDir::new("durable-no-dup");
    let id = ConsequenceId::new(6, 1, 1);
    // One encounter: a durable-classified award (occurrence 1) plus a
    // provisional (occurrence 2); the encounter FAILS.
    let mut failed = EncounterJournal::new(SignalLane::Captured);
    failed
        .begin_participant(id, AwardState::new(), CounterValues::default())
        .unwrap();
    assert!(
        failed
            .record_effect(id, 1, DurabilityClass::DurableEligible)
            .unwrap()
    );
    assert!(
        failed
            .record_effect(id, 2, DurabilityClass::EncounterProvisional)
            .unwrap()
    );
    failed.rollback().unwrap();
    let (award, restored) = failed.participant_state(id).unwrap();
    assert!(
        award.durable.covers(1),
        "the durable award survives the failure"
    );
    assert!(
        !award.covers(2),
        "the provisional occurrence is earnable again"
    );
    assert_eq!(*restored, counters(1, SignalLane::Captured, 1));
    // The reattempt (the same identity — the encounter failed, never
    // committed): occurrence 1 can NEVER re-award; occurrence 2 fires.
    let mut pipeline = fresh_pipeline(&dir);
    let mut reattempt = EncounterJournal::new(SignalLane::Captured);
    reattempt
        .begin_participant(id, award.clone(), *restored)
        .unwrap();
    assert!(
        !reattempt
            .record_effect(id, 1, DurabilityClass::DurableEligible)
            .unwrap(),
        "the durably-retained award is never re-awarded"
    );
    assert!(
        reattempt
            .record_effect(id, 2, DurabilityClass::EncounterProvisional)
            .unwrap()
    );
    reattempt
        .commit(
            &mut pipeline,
            position(130),
            Vec::new(),
            frontier(2),
            b"encounter-reattempt".to_vec(),
        )
        .unwrap()
        .expect("effects were accrued");
    drop(pipeline);
    // Recovery from the anchor representing the retained durable
    // award: occurrence 2 applies exactly once; occurrence 1 is
    // suppressed by the anchor's coverage — never duplicated.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let anchor = anchor_at(125).with_identity(
        id,
        counters(1, SignalLane::Captured, 1),
        durable_award(&[1]),
    );
    let outcome = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    let participant = outcome.recovered.get(&id).unwrap();
    assert_eq!(
        participant.counters,
        counters(2, SignalLane::Captured, 2),
        "occurrence 1 never re-awarded; occurrence 2 exactly once"
    );
    assert!(participant.award.durable.covers(1) && participant.award.durable.covers(2));
}

/// §5.4.1 (9) — repeated crashes during reconciliation followed by
/// canonical confirmation: every cycle re-derives identically; the
/// final publication + T3 confirm exactly-once.
#[test]
fn multi_crash_reconciliation_then_confirmation() {
    let dir = TestDir::new("multi-crash");
    let a = ConsequenceId::new(7, 1, 1);
    let b = ConsequenceId::new(7, 2, 1);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(a, (0, 2), SignalLane::Captured, 110)],
                110,
                1,
            ))
            .unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(b, (2, 4), SignalLane::Captured, 115)],
                115,
                2,
            ))
            .unwrap();
    }
    let anchor = anchor_at(100);
    let mut previous: Option<RecoveryOutcome> = None;
    for cycle in 0..3 {
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        let outcome = reconcile(&anchor, &scan, &no_chain()).unwrap();
        assert_recovery_invariants(&outcome);
        if let Some(previous) = &previous {
            assert_eq!(
                previous, &outcome,
                "crash cycle {cycle} re-derives the identical state"
            );
        }
        previous = Some(outcome);
    }
    let recovered = previous.unwrap();
    assert_eq!(
        recovered.recovered.get(&a).unwrap().counters,
        counters(2, SignalLane::Captured, 2)
    );
    assert_eq!(
        recovered.recovered.get(&b).unwrap().counters,
        counters(2, SignalLane::Captured, 2)
    );
    // The final publication + canonical confirmation: exactly once.
    let mut pipeline = fresh_pipeline(&dir);
    let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
        panic!("expected confirmation");
    };
    assert_eq!(confirmation.keys, vec![a, b]);
    let confirmed: BTreeSet<ConsequenceId> = pipeline.confirmed_keys().into_iter().collect();
    drop(pipeline);
    // Both tiers suppress after publication + confirmation.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let published = anchor_at(120)
        .with_identity(
            a,
            counters(2, SignalLane::Captured, 2),
            durable_award(&[1, 2]),
        )
        .with_identity(
            b,
            counters(2, SignalLane::Captured, 2),
            durable_award(&[3, 4]),
        );
    let outcome = reconcile(&published, &scan, &confirmed).unwrap();
    assert_recovery_invariants(&outcome);
    assert!(outcome.report.applied.is_empty());
    assert!(outcome.report.chain_suppressed.contains(&a));
    assert!(outcome.report.chain_suppressed.contains(&b));
}

/// §5.4.1 (10) — after every recovery in the battery above (here: a
/// mixed-class aggregate, a hole-bearing anchor, and a span-less
/// movement composed with a capture): the consequence-conservation
/// equation holds and the application is atomic (whole-or-nothing per
/// envelope — the world+character atomicity's generic form; the real
/// world integration is B4).
#[test]
fn conservation_and_atomicity_after_every_recovery() {
    // (a) A mixed-class aggregate: occurrences 1–2 durable, 3
    // provisional under one identity — both classes of an actionable
    // committed transaction apply, the whole envelope at once.
    let dir = TestDir::new("battery-mixed");
    let x = ConsequenceId::new(8, 1, 1);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        journal
            .append_transaction(&tx(
                vec![mixed_class_envelope(
                    x,
                    (0, 2),
                    (2, 3),
                    SignalLane::Captured,
                    110,
                )],
                110,
                1,
            ))
            .unwrap();
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let outcome = reconcile(&anchor_at(100), &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    let participant = outcome.recovered.get(&x).unwrap();
    assert_eq!(participant.counters, counters(3, SignalLane::Captured, 3));
    assert!(
        participant.award.durable.covers(1)
            && participant.award.durable.covers(2)
            && participant.award.durable.covers(3)
    );
    // Atomic: the single application entry carries the whole missing
    // set (both class sub-spans) and the full unit delta.
    assert_eq!(outcome.report.applied.len(), 1);
    assert_eq!(outcome.report.applied[0].applied.len(), 2);
    assert_eq!(outcome.report.applied[0].delta.milestones, 3);

    // (b) A hole-bearing anchor: durable {1,3} against span (0,4] —
    // exactly the hole (2) and the beyond (4) apply, composing back
    // to one span.
    let dir = TestDir::new("battery-hole");
    let h = ConsequenceId::new(8, 2, 1);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        journal
            .append_transaction(&tx(
                vec![capture_envelope(h, (0, 4), SignalLane::Captured, 110)],
                110,
                1,
            ))
            .unwrap();
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let anchor = anchor_at(100).with_identity(
        h,
        counters(2, SignalLane::Captured, 2),
        durable_award(&[1, 3]),
    );
    let outcome = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    let participant = outcome.recovered.get(&h).unwrap();
    assert_eq!(
        participant.counters,
        counters(4, SignalLane::Captured, 4),
        "the hole fills without duplicating 1 or 3"
    );
    assert_eq!(
        participant.award.durable.spans().len(),
        1,
        "the filled hole composes to one span"
    );
    assert!(participant.award.durable.covers(4));

    // (c) A span-less movement (an activation commit) composed with a
    // capture under distinct identities: the movement applies
    // whole-or-not by identity, conservation exact.
    let dir = TestDir::new("battery-movement");
    let mover = ConsequenceId::new(8, 3, 1);
    let earner = ConsequenceId::new(8, 4, 1);
    let prior = counters(8, SignalLane::Captured, 8);
    {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        journal
            .append_transaction(&tx(
                vec![
                    movement_envelope(mover, -2, 2, prior, 110),
                    capture_envelope(earner, (0, 1), SignalLane::Captured, 110),
                ],
                110,
                1,
            ))
            .unwrap();
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let anchor = anchor_at(100).with_identity(mover, prior, AwardState::new());
    let outcome = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    let moved = outcome.recovered.get(&mover).unwrap();
    assert_eq!(
        moved.counters,
        CounterValues {
            milestones: 8,
            pending_signals: 0,
            captured: 6,
            credits: 2,
        },
        "the movement applies whole: 2 captured become 2 credits"
    );
    assert_eq!(
        outcome.recovered.get(&earner).unwrap().counters,
        counters(1, SignalLane::Captured, 1)
    );
}

// ------------------------------------------ the pipeline crash windows

/// The window before T1: the staged (in-memory) set dies with the
/// process — no durable trace, no journal frame, honestly unknown to
/// the fresh instance (P0-5).
#[test]
fn crash_before_t1_staging_dies_with_the_process() {
    let dir = TestDir::new("before-t1");
    let id = ConsequenceId::new(1, 1, 1);
    {
        let mut pipeline = fresh_pipeline(&dir);
        pipeline
            .stage(capture_envelope(id, (0, 2), SignalLane::Captured, 110))
            .unwrap();
        assert_eq!(pipeline.status(id).unwrap().phase, KeyPhase::InMemory);
        drop(pipeline);
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(scan.committed.len(), 0);
    let mut pipeline = fresh_pipeline(&dir);
    assert!(
        pipeline.status(id).is_none(),
        "in-memory staging never claims durability across the boundary"
    );
    assert_eq!(
        pipeline.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    );
}

/// The W2 window — after T1, before live application: the envelope
/// is durable; the anchor lacks the effects; recovery replays them
/// exactly once and the re-drive confirms.
#[test]
fn crash_after_t1_before_live_application_recovers_exactly_once() {
    let dir = TestDir::new("after-t1");
    let id = ConsequenceId::new(1, 1, 1);
    {
        let mut pipeline = fresh_pipeline(&dir);
        pipeline
            .stage(capture_envelope(id, (0, 3), SignalLane::Captured, 110))
            .unwrap();
        pipeline
            .capture(
                position(110),
                DurabilityClass::DurableEligible,
                Vec::new(),
                frontier(1),
                b"matrix-plan-1".to_vec(),
            )
            .unwrap();
        assert_eq!(pipeline.status(id).unwrap().phase, KeyPhase::JournaledT1);
        drop(pipeline);
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(scan.committed.len(), 1);
    let anchor = anchor_at(100);
    let outcome = reconcile(&anchor, &scan, &no_chain()).unwrap();
    assert_recovery_invariants(&outcome);
    assert_eq!(
        outcome.recovered.get(&id).unwrap().counters,
        counters(3, SignalLane::Captured, 3)
    );
    let mut pipeline = fresh_pipeline(&dir);
    let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
        panic!("expected confirmation");
    };
    assert_eq!(confirmation.keys, vec![id]);
    assert_eq!(
        pipeline.status(id).unwrap().phase,
        KeyPhase::InConfirmedChain
    );
}

/// The after-HEAD-before-ack window (Unknown, installed): the rename
/// happened, the acknowledgment is lost; the process dies with the
/// attempt unresolved. The fresh instance's session-open chain walk
/// verifies the revision on disk and reports the keys confirmed —
/// verified chain inclusion, never the lost local receipt.
#[test]
fn unknown_window_survives_restart_and_resolves_by_the_chain_walk() {
    let dir = TestDir::new("unknown-restart");
    ensure_founded(&dir);
    let id = ConsequenceId::new(1, 1, 1);
    {
        let journal = T1Journal::open(dir.journal()).unwrap();
        let medium = LocalFileMedium::open_with(
            dir.store(),
            Some(FaultControl::new().arm(FaultPoint::AfterHeadRename, FaultAction::InjectError)),
        )
        .unwrap();
        let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
        pipeline
            .stage(capture_envelope(id, (0, 2), SignalLane::Captured, 110))
            .unwrap();
        pipeline
            .capture(
                position(110),
                DurabilityClass::DurableEligible,
                Vec::new(),
                frontier(1),
                b"matrix-plan-1".to_vec(),
            )
            .unwrap();
        let CommitOutcome::Unknown { .. } = pipeline.commit_canonical().unwrap() else {
            panic!("expected the unknown window");
        };
        assert_eq!(
            pipeline.status(id).unwrap().phase,
            KeyPhase::JournaledT1,
            "an unknown outcome is never a confirmation"
        );
        drop(pipeline);
    }
    // The restart: the walk verifies the installed revision — the
    // keys are confirmed by the chain, not by the lost receipt.
    let pipeline = fresh_pipeline(&dir);
    assert_eq!(
        pipeline.status(id).unwrap().phase,
        KeyPhase::InConfirmedChain
    );
    assert!(pipeline.is_confirmed(id));
    assert_eq!(pipeline.confirmed_coverage(id).unwrap().len(), 1);
    let confirmed: BTreeSet<ConsequenceId> = pipeline.confirmed_keys().into_iter().collect();
    assert!(confirmed.contains(&id));
    drop(pipeline);
    // Recovery against the confirmed set: tier-1 suppression.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let outcome = reconcile(&anchor_at(100), &scan, &confirmed).unwrap();
    assert_recovery_invariants(&outcome);
    assert!(outcome.report.applied.is_empty());
    assert!(outcome.report.chain_suppressed.contains(&id));
    let mut check = LocalFileMedium::open(dir.store()).unwrap();
    assert_eq!(check.validate().unwrap().records, 2);
}
