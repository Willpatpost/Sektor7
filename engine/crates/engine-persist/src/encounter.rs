//! The generic, policy-free encounter machinery (`encounter.rs`, the
//! fifth of the six Batch 3 modules) — the ProvisionalEncounter state
//! of the five-state model, executed over the committed
//! consequence/envelope/journal/pipeline stack.
//!
//! **Policy-free (the binding boundary):** the engine NEVER decides
//! which consequences are durable or provisional — the application
//! supplies the classification per effect; the machinery faithfully
//! executes it. Gameplay determines which consumables, equipment,
//! proficiency, or quest rewards survive an encounter failure; the
//! engine records and enforces the classification.
//!
//! **The transaction (§5.4.1's v1.8 rollback table, at the encounter
//! level):** `begin_participant` checkpoints each participant's
//! `AwardState` (both lanes) and counters; `record_effect` applies the
//! uniform per-occurrence unit (+1 milestone, +1 signal into the
//! encounter's lane) under the supplied classification — an
//! idempotent set-insert: an already-covered occurrence is never a
//! second award; `promote` is the explicit, idempotent
//! provisional→durable re-classification (covering the occurrence
//! durably whether or not it was provisional).
//!
//! **The binding invariant (the reviewer's regression):** on failure,
//! `rollback` restores the checkpoint's provisional coverage **minus
//! every occurrence the durable lane has covered since** — a durable
//! promotion survives per its classification and is **never
//! reintroduced** to the provisional lane (one occurrence, two lanes,
//! is an invalid state); the discarded provisional occurrences become
//! legitimately earnable again (the re-attempt); the durable lane
//! never rolls back. Exact interval membership throughout, cross-lane
//! disjointness verified, legitimate rollback holes preserved (never
//! treated as covered). The counters restore to the checkpoint plus
//! the units of occurrences newly durable **whose units this
//! encounter recorded** — a pre-existing provisional occurrence
//! promoted during the encounter keeps its checkpoint counters and is
//! never double-counted.
//!
//! **Commit (the durability path):** the encounter's net new coverage
//! since the checkpoint is journaled as one boundary transaction —
//! one envelope per participant carrying the **class-homogeneous
//! sub-span partition** (the durable-lane difference and the
//! provisional-lane difference as separately tagged spans — a mixed
//! aggregate, never one class), staged into the pipeline and captured
//! (the receipt only after the journal's fsync); the live states then
//! fold (provisional → durable). The canonical confirmation is the
//! pipeline's separate `commit_canonical` — the encounter's effects
//! become `JournaledT1`, honestly pending. **A failed boundary leaves
//! nothing staged behind** (the uncaptured envelopes are in-memory
//! pending only, discarded on the error path); a retried commit
//! re-derives identical envelopes — the pipeline's byte-identical
//! restage is a no-op, one transaction, coherent identity and coverage
//! across the retry. Both commit and rollback close the encounter; a
//! re-attempt is a new encounter journal (the identity discipline:
//! participant identities are commit-once — a re-attempt after
//! rollback re-derives the same identity; after a commit the episode
//! advances).

use std::collections::BTreeMap;
use std::fmt;

use engine_core::SaveError;

use crate::consequence::{AwardCheckpoint, AwardState, ConsequenceId, Coverage, OccurrenceSpan};
use crate::envelope::{
    ClassifiedSpan, ConsequenceEnvelope, CounterDelta, CounterValues, DurabilityClass, SignalLane,
    SourceBaseline, SourcePosition,
};
use crate::journal::{FrontierEntry, TransactionId};
use crate::medium::CanonicalMedium;
use crate::pipeline::{CaptureReceipt, CommitPipeline, PipelineError};

/// One participant's encounter-transaction state: the begin
/// checkpoint (both award lanes + the counters) and the live state
/// the machinery mutates.
struct Participant {
    checkpoint: AwardCheckpoint,
    checkpoint_counters: CounterValues,
    live: AwardState,
    live_counters: CounterValues,
}

/// Typed encounter failures.
#[derive(Debug)]
pub enum EncounterError {
    Format(SaveError),
    Pipeline(PipelineError),
    UnknownParticipant {
        id: ConsequenceId,
    },
    DuplicateParticipant {
        id: ConsequenceId,
    },
    /// The encounter is finished (committed or rolled back); a
    /// re-attempt is a new encounter journal.
    EncounterClosed,
}

impl fmt::Display for EncounterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncounterError::Format(error) => write!(f, "encounter format failure: {error}"),
            EncounterError::Pipeline(error) => write!(f, "encounter durability failure: {error}"),
            EncounterError::UnknownParticipant { id } => {
                write!(f, "encounter participant {id} was never begun")
            }
            EncounterError::DuplicateParticipant { id } => {
                write!(f, "encounter participant {id} was already begun")
            }
            EncounterError::EncounterClosed => write!(
                f,
                "the encounter is finished — a re-attempt is a new encounter journal"
            ),
        }
    }
}

impl std::error::Error for EncounterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EncounterError::Format(error) => Some(error),
            EncounterError::Pipeline(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SaveError> for EncounterError {
    fn from(error: SaveError) -> Self {
        EncounterError::Format(error)
    }
}

impl From<PipelineError> for EncounterError {
    fn from(error: PipelineError) -> Self {
        EncounterError::Pipeline(error)
    }
}

/// The policy-free encounter transaction machinery.
pub struct EncounterJournal {
    /// The uniform signal lane of this encounter's per-occurrence
    /// units (the application's boundary choice — the envelope's
    /// unit purity).
    lane: SignalLane,
    participants: BTreeMap<ConsequenceId, Participant>,
    finished: bool,
}

impl EncounterJournal {
    /// Begins an encounter whose effects carry the uniform
    /// per-occurrence unit into `lane`.
    pub fn new(lane: SignalLane) -> Self {
        EncounterJournal {
            lane,
            participants: BTreeMap::new(),
            finished: false,
        }
    }

    /// Checkpoints one participant (both award lanes + the counters).
    /// The supplied state must be cross-lane coherent (disjoint) —
    /// an incoherent begin fails closed, never repaired.
    pub fn begin_participant(
        &mut self,
        id: ConsequenceId,
        award: AwardState,
        counters: CounterValues,
    ) -> Result<(), EncounterError> {
        self.require_open()?;
        award.validate()?;
        if self.participants.contains_key(&id) {
            return Err(EncounterError::DuplicateParticipant { id });
        }
        let checkpoint = award.checkpoint();
        self.participants.insert(
            id,
            Participant {
                checkpoint,
                checkpoint_counters: counters,
                live: award,
                live_counters: counters,
            },
        );
        Ok(())
    }

    /// Records one classified effect at `occurrence` (occurrences
    /// number from 1): applies the uniform unit (+1 milestone, +1
    /// signal into the encounter's lane) and inserts the occurrence
    /// into the classified lane — an idempotent set operation; an
    /// already-covered occurrence (either lane) is never a second
    /// award (returns `false`). The classification is the
    /// application's policy; the machinery executes it.
    pub fn record_effect(
        &mut self,
        id: ConsequenceId,
        occurrence: u64,
        class: DurabilityClass,
    ) -> Result<bool, EncounterError> {
        let lane = self.lane;
        let participant = self.open_participant(id)?;
        if occurrence == 0 {
            return Err(EncounterError::Format(SaveError::Integrity(
                "encounter/occurrence-zero",
            )));
        }
        if participant.live.covers(occurrence) {
            return Ok(false);
        }
        let unit = CounterDelta::uniform(lane, 1)?;
        participant.live_counters = unit.apply_to(&participant.live_counters)?;
        match class {
            DurabilityClass::DurableEligible => participant.live.award_durable(occurrence),
            DurabilityClass::EncounterProvisional => participant.live.award_provisional(occurrence),
        }
        Ok(true)
    }

    /// The explicit, idempotent mid-encounter durable promotion: the
    /// occurrence joins the durable lane and leaves the provisional
    /// lane — **and survives a later rollback per its
    /// classification** (never reintroduced to the provisional lane,
    /// never re-awarded on a re-attempt). Returns whether a
    /// provisional occurrence actually moved; promoting an uncovered
    /// occurrence classifies it durable directly (the application's
    /// explicit decision).
    pub fn promote(&mut self, id: ConsequenceId, occurrence: u64) -> Result<bool, EncounterError> {
        let participant = self.open_participant(id)?;
        if occurrence == 0 {
            return Err(EncounterError::Format(SaveError::Integrity(
                "encounter/occurrence-zero",
            )));
        }
        let moved = participant.live.provisional.covers(occurrence);
        participant.live.promote_provisional(
            &OccurrenceSpan::new(occurrence - 1, occurrence).map_err(|_| {
                EncounterError::Format(SaveError::Integrity("encounter/promotion-span"))
            })?,
        );
        Ok(moved)
    }

    /// Encounter failure: discards the provisional effects (restoring
    /// eligibility — the legitimate re-attempt re-earns them) while
    /// every durable-classified contribution and promotion survives.
    /// The provisional coverage restores from the checkpoint **minus
    /// every occurrence the durable lane has covered since** (one
    /// occurrence, two lanes, is an invalid state); the counters
    /// restore to the checkpoint plus the units of occurrences newly
    /// durable whose units this encounter recorded (a pre-existing
    /// provisional occurrence promoted during the encounter keeps its
    /// checkpoint counters — never double-counted).
    pub fn rollback(&mut self) -> Result<(), EncounterError> {
        self.require_open()?;
        let lane = self.lane;
        for participant in self.participants.values_mut() {
            let mut new_units =
                coverage_difference(&participant.live.durable, &participant.checkpoint.durable);
            for span in participant.checkpoint.provisional.spans() {
                new_units.remove_span(span);
            }
            let count = coverage_occurrences(&new_units)?;
            let unit = CounterDelta::uniform(lane, count)?;
            participant.live_counters = unit.apply_to(&participant.checkpoint_counters)?;
            participant.live.rollback(participant.checkpoint.clone());
        }
        self.finished = true;
        Ok(())
    }

    /// Commits the encounter's eligible effects through the durability
    /// path: one envelope per participant with net-new coverage — the
    /// **class-homogeneous sub-span partition** (the durable-lane and
    /// provisional-lane differences since the checkpoint, separately
    /// tagged) — staged into the pipeline and captured as one boundary
    /// transaction (the encounter transaction's classification is
    /// `EncounterProvisional`; the per-span classes carry the
    /// policy). The live states fold (provisional → durable). The
    /// canonical confirmation is the pipeline's separate
    /// `commit_canonical`. `None` when no participant accrued net-new
    /// coverage (nothing journaled). A failed boundary discards the
    /// uncaptured staging (in-memory pending only) — the retry
    /// re-derives identical envelopes.
    pub fn commit<M: CanonicalMedium>(
        &mut self,
        pipeline: &mut CommitPipeline<M>,
        position: SourcePosition,
        dependencies: Vec<TransactionId>,
        frontier: Vec<FrontierEntry>,
        plan: Vec<u8>,
    ) -> Result<Option<CaptureReceipt>, EncounterError> {
        self.require_open()?;
        let mut staged_ids: Vec<ConsequenceId> = Vec::new();
        for (id, participant) in &self.participants {
            let Some(envelope) = self.build_envelope(id, participant, &position)? else {
                continue;
            };
            if let Err(error) = pipeline.stage(envelope) {
                pipeline.discard_staged(&staged_ids);
                return Err(error.into());
            }
            staged_ids.push(*id);
        }
        let receipt = if staged_ids.is_empty() {
            self.finished = true;
            return Ok(None);
        } else {
            match pipeline.capture(
                position,
                DurabilityClass::EncounterProvisional,
                dependencies,
                frontier,
                plan,
            ) {
                Ok(receipt) => receipt,
                Err(error) => {
                    // The failed boundary leaves nothing staged behind:
                    // the uncaptured envelopes are in-memory pending
                    // only (P0-5) — the retry re-stages identical bytes.
                    pipeline.discard_staged(&staged_ids);
                    return Err(error.into());
                }
            }
        };
        for participant in self.participants.values_mut() {
            participant.live.commit_encounter();
        }
        self.finished = true;
        Ok(Some(receipt))
    }

    /// One participant's live state (readable in every encounter
    /// state, including after commit and rollback).
    pub fn participant_state(&self, id: ConsequenceId) -> Option<(&AwardState, &CounterValues)> {
        self.participants
            .get(&id)
            .map(|participant| (&participant.live, &participant.live_counters))
    }

    /// Whether the encounter is finished (committed or rolled back).
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    // ---------------------------------------------------------- internals

    fn open_participant(&mut self, id: ConsequenceId) -> Result<&mut Participant, EncounterError> {
        self.require_open()?;
        self.participants
            .get_mut(&id)
            .ok_or(EncounterError::UnknownParticipant { id })
    }

    fn require_open(&self) -> Result<(), EncounterError> {
        if self.finished {
            Err(EncounterError::EncounterClosed)
        } else {
            Ok(())
        }
    }

    /// The participant's commit envelope: the net-new coverage since
    /// the checkpoint as the class-homogeneous sub-span partition,
    /// with the checkpoint as the expected-prior and the verified
    /// source baseline. `None` when nothing accrued.
    fn build_envelope(
        &self,
        id: &ConsequenceId,
        participant: &Participant,
        position: &SourcePosition,
    ) -> Result<Option<ConsequenceEnvelope>, EncounterError> {
        let new_durable =
            coverage_difference(&participant.live.durable, &participant.checkpoint.durable);
        let new_provisional = coverage_difference(
            &participant.live.provisional,
            &participant.checkpoint.provisional,
        );
        if new_durable.is_empty() && new_provisional.is_empty() {
            return Ok(None);
        }
        let mut sub_spans: Vec<ClassifiedSpan> = Vec::new();
        for span in new_durable.spans() {
            sub_spans.push(ClassifiedSpan {
                span: *span,
                class: DurabilityClass::DurableEligible,
            });
        }
        for span in new_provisional.spans() {
            sub_spans.push(ClassifiedSpan {
                span: *span,
                class: DurabilityClass::EncounterProvisional,
            });
        }
        sub_spans.sort_unstable_by_key(|classified| classified.span.lo);
        let envelope = ConsequenceEnvelope::capture(
            *id,
            sub_spans,
            self.lane,
            participant.checkpoint_counters,
            SourceBaseline {
                counters: participant.checkpoint_counters,
                award: participant.checkpoint.clone(),
            },
            *position,
        )?;
        Ok(Some(envelope))
    }
}

/// The set difference `live − base` (exact interval arithmetic — the
/// occurrences covered by `live` but not `base`).
fn coverage_difference(live: &Coverage, base: &Coverage) -> Coverage {
    let mut difference = live.clone();
    for span in base.spans() {
        difference.remove_span(span);
    }
    difference
}

/// The total occurrences covered (checked — overflow fails closed).
fn coverage_occurrences(coverage: &Coverage) -> Result<u64, SaveError> {
    let mut total: u64 = 0;
    for span in coverage.spans() {
        total = total
            .checked_add(span.hi - span.lo)
            .ok_or(SaveError::Integrity("encounter/occurrence-overflow"))?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault::FaultAction;
    use crate::journal::{JournalFaultControl, JournalFaultPoint, T1Journal};
    use crate::local::LocalFileMedium;
    use crate::medium::AppendOutcome;
    use crate::pipeline::CommitOutcome;
    use crate::record::{AuthorityMode, RevisionRecord};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("s7-e5-unit-{}-{label}-{seq}", std::process::id()));
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

    fn ensure_founded(dir: &TestDir) {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        if medium.head().unwrap().is_some() {
            return;
        }
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "encounter-campaign", 0);
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            other => panic!("the founding must accept: {other:?}"),
        }
    }

    fn fresh_pipeline(dir: &TestDir) -> crate::pipeline::CommitPipeline<LocalFileMedium> {
        ensure_founded(dir);
        let journal = T1Journal::open(dir.journal()).unwrap();
        crate::pipeline::CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal)
            .unwrap()
    }

    fn participant_id() -> ConsequenceId {
        ConsequenceId::new(1, 1, 1)
    }

    struct CommitBoundary {
        position: SourcePosition,
        dependencies: Vec<TransactionId>,
        frontier: Vec<FrontierEntry>,
        plan: Vec<u8>,
    }

    fn commit_boundary() -> CommitBoundary {
        CommitBoundary {
            position: SourcePosition {
                head: None,
                tick: 200,
            },
            dependencies: Vec::new(),
            frontier: vec![FrontierEntry {
                region: 1,
                episode: 1,
            }],
            plan: b"encounter-plan".to_vec(),
        }
    }

    // ------------------------------------------------------ the flow

    #[test]
    fn begin_record_commit_journals_and_folds() {
        let dir = TestDir::new("end-to-end");
        let mut pipeline = fresh_pipeline(&dir);
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        assert!(
            encounter
                .record_effect(id, 4, DurabilityClass::EncounterProvisional)
                .unwrap()
        );
        assert!(
            encounter
                .record_effect(id, 5, DurabilityClass::DurableEligible)
                .unwrap()
        );

        let boundary = commit_boundary();
        let receipt = encounter
            .commit(
                &mut pipeline,
                boundary.position,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap()
            .expect("effects were accrued");
        assert!(!receipt.reconciled);
        assert!(encounter.is_finished());
        // The effects are JournaledT1 — honestly pending, not confirmed.
        assert_eq!(
            pipeline.status(id).unwrap().phase,
            crate::pipeline::KeyPhase::JournaledT1
        );
        // The live state folded: provisional emptied, durable holds
        // both occurrences, cross-lane coherent.
        let (award, counters) = encounter.participant_state(id).unwrap();
        assert!(award.durable.covers(4) && award.durable.covers(5));
        assert!(award.provisional.is_empty());
        assert_eq!(counters.milestones, 2);
        assert_eq!(counters.captured, 2);
        assert!(counters.conservation_holds());
        // The canonical confirmation is the pipeline's separate act.
        let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(confirmation.keys, vec![id]);
        assert_eq!(
            pipeline.status(id).unwrap().phase,
            crate::pipeline::KeyPhase::InConfirmedChain
        );
    }

    #[test]
    fn checkpoint_promote_fail_rollback_keeps_promotion_durable_only() {
        // THE reviewer's regression, at the transaction level: the
        // checkpoint holds provisional {5}; the encounter promotes 5
        // to durable; the encounter FAILS — 5 stays durable-only
        // (never reintroduced to the provisional lane), the new
        // provisional effect becomes earnable again, the counters
        // keep 5's unit and discard the failed effect's.
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        let mut begin = AwardState::new();
        begin.award_provisional(5);
        let begin_counters = CounterDelta::uniform(SignalLane::Captured, 1)
            .unwrap()
            .apply_to(&CounterValues::default())
            .unwrap();
        encounter
            .begin_participant(id, begin, begin_counters)
            .unwrap();

        assert!(
            encounter
                .record_effect(id, 7, DurabilityClass::EncounterProvisional)
                .unwrap()
        );
        assert!(
            encounter.promote(id, 5).unwrap(),
            "5 was provisional — the promotion moves it"
        );
        encounter.rollback().unwrap();

        let (award, counters) = encounter.participant_state(id).unwrap();
        assert!(
            award.durable.covers(5),
            "the promotion survives the failure"
        );
        assert!(
            !award.provisional.covers(5),
            "the promoted occurrence is never reintroduced to the provisional lane"
        );
        assert!(
            !award.covers(7),
            "the failed provisional effect is earnable again"
        );
        assert_eq!(counters.milestones, 1, "5's unit retained, 7's discarded");
        assert_eq!(counters.captured, 1);
        assert!(counters.conservation_holds());
        // Cross-lane disjoint and serializable (the coherent state).
        assert!(award.validate().is_ok());
        assert!(award.canonical_bytes().is_ok());
    }

    #[test]
    fn mixed_checkpoint_partial_promotion_rollback() {
        // The c68061f shape at the transaction level: checkpoint
        // provisional {5, 7}; only 5 promoted → rollback restores
        // provisional {7} with 5 durable-only.
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        let mut begin = AwardState::new();
        begin.award_provisional(5);
        begin.award_provisional(7);
        let begin_counters = CounterDelta::uniform(SignalLane::Captured, 2)
            .unwrap()
            .apply_to(&CounterValues::default())
            .unwrap();
        encounter
            .begin_participant(id, begin, begin_counters)
            .unwrap();
        assert!(encounter.promote(id, 5).unwrap());
        encounter.rollback().unwrap();

        let (award, counters) = encounter.participant_state(id).unwrap();
        assert!(award.durable.covers(5) && !award.provisional.covers(5));
        assert!(award.provisional.covers(7) && !award.durable.covers(7));
        assert_eq!(
            counters.milestones, 2,
            "both checkpoint units retained — no double-count for the promoted 5"
        );
        assert!(award.validate().is_ok());
    }

    #[test]
    fn rollback_re_attempt_re_earns_without_re_awarding_durable() {
        // After the failure, the re-attempt legitimately re-earns the
        // discarded provisional occurrence — and never re-awards the
        // durable one.
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        encounter
            .record_effect(id, 4, DurabilityClass::DurableEligible)
            .unwrap();
        encounter
            .record_effect(id, 5, DurabilityClass::EncounterProvisional)
            .unwrap();
        encounter.rollback().unwrap();
        let (award, _) = encounter.participant_state(id).unwrap();
        assert!(award.durable.covers(4), "the durable effect survives");
        assert!(!award.covers(5), "the provisional effect is earnable again");

        // The re-attempt is a new encounter over the rolled-back state.
        let (rolled_back, counters) = encounter.participant_state(id).unwrap();
        let mut re_attempt = EncounterJournal::new(SignalLane::Captured);
        re_attempt
            .begin_participant(id, rolled_back.clone(), *counters)
            .unwrap();
        assert!(
            re_attempt
                .record_effect(id, 5, DurabilityClass::EncounterProvisional)
                .unwrap(),
            "the legitimate re-earn"
        );
        assert!(
            !re_attempt
                .record_effect(id, 4, DurabilityClass::DurableEligible)
                .unwrap(),
            "the durable occurrence is never re-awarded"
        );
        let (award, counters) = re_attempt.participant_state(id).unwrap();
        assert!(award.durable.covers(4) && award.provisional.covers(5));
        assert_eq!(
            counters.milestones, 2,
            "one unit per occurrence — never two"
        );
        assert!(counters.conservation_holds());
    }

    #[test]
    fn record_is_idempotent_no_second_award() {
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        assert!(
            encounter
                .record_effect(id, 4, DurabilityClass::EncounterProvisional)
                .unwrap()
        );
        assert!(
            !encounter
                .record_effect(id, 4, DurabilityClass::EncounterProvisional)
                .unwrap()
        );
        // Re-recording under the other classification is also a no-op:
        // the class change is the explicit promotion, never a second
        // award.
        assert!(
            !encounter
                .record_effect(id, 4, DurabilityClass::DurableEligible)
                .unwrap()
        );
        let (_, counters) = encounter.participant_state(id).unwrap();
        assert_eq!(
            counters.milestones, 1,
            "one unit — the same occurrence never counts twice"
        );
    }

    #[test]
    fn promote_semantics() {
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        // Promoting an uncovered occurrence classifies it durable
        // directly (the application's explicit decision).
        assert!(
            !encounter.promote(id, 3).unwrap(),
            "nothing moved — 3 was uncovered"
        );
        let (award, _) = encounter.participant_state(id).unwrap();
        assert!(award.durable.covers(3));
        // Idempotent: promoting again is a no-op.
        assert!(!encounter.promote(id, 3).unwrap());
        // Promoting a provisional occurrence moves it.
        encounter
            .record_effect(id, 5, DurabilityClass::EncounterProvisional)
            .unwrap();
        assert!(encounter.promote(id, 5).unwrap());
        let (award, _) = encounter.participant_state(id).unwrap();
        assert!(award.durable.covers(5) && !award.provisional.covers(5));
        assert!(award.validate().is_ok());
    }

    #[test]
    fn cross_lane_disjoint_throughout() {
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        let mut begin = AwardState::new();
        begin.award_provisional(5);
        encounter
            .begin_participant(id, begin, CounterValues::default())
            .unwrap();
        for (occurrence, class) in [
            (4, DurabilityClass::DurableEligible),
            (6, DurabilityClass::EncounterProvisional),
            (8, DurabilityClass::DurableEligible),
        ] {
            encounter.record_effect(id, occurrence, class).unwrap();
            encounter.promote(id, 6).unwrap();
            let (award, _) = encounter.participant_state(id).unwrap();
            assert!(
                award.validate().is_ok(),
                "cross-lane disjoint after every operation"
            );
        }
        encounter.rollback().unwrap();
        let (award, _) = encounter.participant_state(id).unwrap();
        assert!(award.validate().is_ok());
        assert!(award.canonical_bytes().is_ok());
    }

    #[test]
    fn legitimate_rollback_holes_are_preserved() {
        // Durable {1, 3} with the provisional 2 discarded: the hole at
        // 2 is a legitimate rollback hole — preserved exactly, never
        // treated as covered.
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        encounter
            .record_effect(id, 1, DurabilityClass::DurableEligible)
            .unwrap();
        encounter
            .record_effect(id, 2, DurabilityClass::EncounterProvisional)
            .unwrap();
        encounter
            .record_effect(id, 3, DurabilityClass::DurableEligible)
            .unwrap();
        encounter.rollback().unwrap();
        let (award, _) = encounter.participant_state(id).unwrap();
        assert!(award.durable.covers(1) && award.durable.covers(3));
        assert!(
            !award.covers(2),
            "the hole at 2 is preserved — earnable again"
        );
        // The durable spans remain separate intervals (the hole
        // between them is real).
        assert_eq!(award.durable.spans().len(), 2);
    }

    #[test]
    fn mixed_class_partition_envelope_and_baseline() {
        // One participant, occurrences 4 durable / 5 provisional /
        // 6 durable: the commit envelope carries the class-homogeneous
        // partition — adjacent cross-class sub-spans, never one class
        // for the mixed aggregate — with the checkpoint as the
        // expected-prior and baseline.
        let dir = TestDir::new("mixed-partition");
        let mut pipeline = fresh_pipeline(&dir);
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        encounter
            .record_effect(id, 4, DurabilityClass::DurableEligible)
            .unwrap();
        encounter
            .record_effect(id, 5, DurabilityClass::EncounterProvisional)
            .unwrap();
        encounter
            .record_effect(id, 6, DurabilityClass::DurableEligible)
            .unwrap();
        let boundary = commit_boundary();
        encounter
            .commit(
                &mut pipeline,
                boundary.position,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap()
            .expect("effects were accrued");

        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert_eq!(scan.committed.len(), 1);
        let transaction = &scan.committed[0];
        assert_eq!(
            transaction.classification,
            DurabilityClass::EncounterProvisional
        );
        let envelope = &transaction.envelopes[0];
        assert_eq!(
            envelope.sub_spans.len(),
            3,
            "three class-homogeneous sub-spans"
        );
        assert_eq!(envelope.sub_spans[0].span.lo, 3);
        assert_eq!(
            envelope.sub_spans[0].class,
            DurabilityClass::DurableEligible
        );
        assert_eq!(
            envelope.sub_spans[1].class,
            DurabilityClass::EncounterProvisional
        );
        assert_eq!(
            envelope.sub_spans[2].class,
            DurabilityClass::DurableEligible
        );
        assert_eq!(
            envelope.delta.milestones, 3,
            "the uniform unit x three occurrences"
        );
        // The baseline is the begin checkpoint (the verified source
        // baseline at preparation).
        assert_eq!(envelope.baseline.counters, CounterValues::default());
        assert!(envelope.baseline.award.durable.is_empty());
    }

    #[test]
    fn commit_with_no_accrued_effects_journals_nothing() {
        let dir = TestDir::new("no-effects");
        let mut pipeline = fresh_pipeline(&dir);
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        let boundary = commit_boundary();
        assert!(
            encounter
                .commit(
                    &mut pipeline,
                    boundary.position,
                    boundary.dependencies,
                    boundary.frontier,
                    boundary.plan
                )
                .unwrap()
                .is_none()
        );
        assert!(encounter.is_finished());
        assert_eq!(
            T1Journal::scan_file(dir.journal()).unwrap().committed.len(),
            0
        );
    }

    #[test]
    fn finished_encounter_rejects_operations() {
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        encounter.rollback().unwrap();
        assert!(matches!(
            encounter.record_effect(id, 4, DurabilityClass::DurableEligible),
            Err(EncounterError::EncounterClosed)
        ));
        assert!(matches!(
            encounter.promote(id, 4),
            Err(EncounterError::EncounterClosed)
        ));
        assert!(matches!(
            encounter.rollback(),
            Err(EncounterError::EncounterClosed)
        ));
    }

    #[test]
    fn participant_validation_and_occurrence_zero() {
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        assert!(matches!(
            encounter.record_effect(id, 4, DurabilityClass::DurableEligible),
            Err(EncounterError::UnknownParticipant { .. })
        ));
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        assert!(matches!(
            encounter.record_effect(id, 0, DurabilityClass::DurableEligible),
            Err(EncounterError::Format(_))
        ));
        assert!(matches!(
            encounter.promote(id, 0),
            Err(EncounterError::Format(_))
        ));
        assert!(matches!(
            encounter.begin_participant(id, AwardState::new(), CounterValues::default()),
            Err(EncounterError::DuplicateParticipant { .. })
        ));
        // An incoherent begin (the same occurrence in both lanes) fails
        // closed — the award methods cannot construct it, so the state
        // is built directly (decode rejects it; so does the machinery).
        let mut durable = Coverage::new();
        durable.insert_span(OccurrenceSpan::new(3, 4).unwrap());
        let mut provisional = Coverage::new();
        provisional.insert_span(OccurrenceSpan::new(3, 4).unwrap());
        let incoherent = AwardState {
            durable,
            provisional,
        };
        assert!(matches!(
            encounter.begin_participant(
                ConsequenceId::new(1, 2, 1),
                incoherent,
                CounterValues::default()
            ),
            Err(EncounterError::Format(_))
        ));
    }

    #[test]
    fn failed_commit_discards_staging_and_retry_is_coherent() {
        // The boundary journal write fails after staging: nothing is
        // left staged (in-memory pending only, honestly discarded);
        // the retry through a clean pipeline re-derives identical
        // envelopes — one transaction, coherent identity and
        // coverage.
        let dir = TestDir::new("failed-commit-retry");
        ensure_founded(&dir);
        let journal = T1Journal::open_with(
            dir.journal(),
            Some(
                JournalFaultControl::new()
                    .arm(JournalFaultPoint::CapturesWritten, FaultAction::InjectError),
            ),
        )
        .unwrap();
        let mut faulted = crate::pipeline::CommitPipeline::open(
            LocalFileMedium::open(dir.store()).unwrap(),
            journal,
        )
        .unwrap();
        let mut encounter = EncounterJournal::new(SignalLane::Captured);
        let id = participant_id();
        encounter
            .begin_participant(id, AwardState::new(), CounterValues::default())
            .unwrap();
        encounter
            .record_effect(id, 4, DurabilityClass::EncounterProvisional)
            .unwrap();

        let boundary = commit_boundary();
        assert!(matches!(
            encounter.commit(
                &mut faulted,
                boundary.position,
                boundary.dependencies.clone(),
                boundary.frontier.clone(),
                boundary.plan.clone()
            ),
            Err(EncounterError::Pipeline(_))
        ));
        assert!(
            !encounter.is_finished(),
            "the encounter remains open for the retry"
        );
        // The uncaptured staging was discarded — no in-memory pending
        // trace (P0-5).
        assert!(faulted.status(id).is_none());
        assert_eq!(
            T1Journal::scan_file(dir.journal()).unwrap().committed.len(),
            0
        );
        // The journal's flock is held for the faulted pipeline's
        // lifetime — drop it before opening the clean instance.
        drop(faulted);

        // The retry through a clean pipeline: identical envelopes, one
        // transaction, the encounter still open until it succeeds.
        let mut clean = fresh_pipeline(&dir);
        let receipt = encounter
            .commit(
                &mut clean,
                boundary.position,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap()
            .expect("the retry commits");
        assert!(!receipt.reconciled);
        assert!(encounter.is_finished());
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert_eq!(scan.committed.len(), 1, "one transaction — no duplicate");
        assert_eq!(scan.committed[0].envelopes.len(), 1);
        assert_eq!(
            clean.status(id).unwrap().phase,
            crate::pipeline::KeyPhase::JournaledT1
        );
        let (award, _) = encounter.participant_state(id).unwrap();
        assert!(award.durable.covers(4), "the folded live state");
        assert!(award.provisional.is_empty());
    }
}
