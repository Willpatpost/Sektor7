//! The two-contract recovery (`recovery.rs`, the sixth and final
//! Batch 3 module) — §5.4.1's recovery model as the deterministic
//! pure function of (anchor, journal, confirmed chain).
//!
//! **Contract A — consequence recovery (unconditional for every
//! actionable durable transaction):** each envelope's still-missing
//! occurrences — computed exactly by
//! [`ConsequenceEnvelope::missing_against`] against the working
//! durable coverage (the anchor's materialized coverage plus this
//! run's in-run applications) — apply **atomically and exactly-once
//! by identity**: the counters advance by the uniform unit × the
//! missing count, and the missing spans promote into the durable lane
//! (clearing the provisional lane — cross-lane coherence preserved).
//! An actionable transaction (journal-committed **and uncanceled**) is
//! the durable commit evidence of its encounter's acceptance: a
//! terminated (failed) encounter either never journaled or was
//! canceled — the v1.8 rule that EncounterProvisional intents are
//! never promoted after a terminated encounter holds structurally.
//! Recovery never fabricates simulation steps or transient gameplay
//! history: the only restorable values are the envelope deltas (the
//! durable records of real, verified executions' consequences) —
//! `steps` and routine counters are structurally absent.
//!
//! **The four-tier suppression (the reviewer's binding rule):** T1
//! journal presence NEVER suppresses a missing effect. Only (1) the
//! **verified canonical inclusion** (the chain-confirmed identity
//! set, supplied by the pipeline) and (2) the **materialized snapshot
//! coverage** (the anchor's durable lane — and the in-run
//! applications, which die with the process and re-derive
//! identically on restart) establish suppression. Legitimate rollback
//! holes remain recoverable without duplicate application: exact
//! interval membership means an anchor covering {4, 6} against the
//! span `(3, 7]` yields exactly the missing {5, 7} — the hole fills,
//! nothing double-applies.
//!
//! **Contract B — exact transition recovery (conditional via the
//! temporal discriminator):** the anchor predates the transaction's
//! source position (anchor tick ≤ source tick) ⇒ **benign absence** —
//! Contract A proceeds unconditionally and the transition is marked
//! `BenignAbsence` (consequence-recovered, transition not
//! reconstructed — the encounter-boundary path; B4 owns the actual
//! reconstruction). The anchor at-or-after the source ⇒ the
//! comparable-state check. **Individual signal lanes are never
//! compared** — valid captures transfer pending → captured and
//! activations transfer captured → credits, so a later, causally
//! valid anchor may hold less of an individual lane than the
//! envelope's own units added; the witnesses are the milestone
//! counter (awards only add, per identity) and the occurrence
//! coverage (either lane): a coherent anchor holds exactly the
//! baseline milestones plus the newly-materialized occurrences'
//! units — fewer milestones than the coverage materializes, or more
//! milestones than any lane explains, are contradictions: **fail
//! closed** ([`RecoveryError::Incompatible`], evidence preserved). A
//! fully materialized transition (the durable coverage covers every
//! occurrence) applies nothing — no duplicate. **Span-less movements
//! apply whole-or-not by identity only** — the durable suppression
//! authorities are the chain (tier 1) and the snapshot's identity-
//! exact materialization evidence (the per-identity transaction set,
//! published atomically WITH the effects: a materialized movement is
//! never reapplied in the W6 window); counter equality is never
//! evidence of an applied identity (unrelated operations can
//! produce identical values, and two journaled movements with
//! identical deltas each apply exactly once per run).
//!
//! **Purity and idempotence (the W4 property):** `reconcile` is a
//! pure function of its inputs — no durable intermediate ever claims
//! representation, so a crash before the next snapshot publication
//! simply re-derives the identical result; the recovered state
//! becomes durable in ONE atomic operation, the caller's next
//! snapshot/revision publication. The canonical re-drive is likewise
//! the caller's separate act (`CommitPipeline::commit_canonical`
//! batches the unconfirmed journal keys under their original
//! identities) — recovery never falsely confirms.
//!
//! **The conservation invariant** holds after every application (the
//! unit is balanced: +1 milestone, +1 signal); a violation fails
//! closed. **The monotonic identity frontier**
//! ([`reconstruct_episode_frontier`]) advances beyond every durably
//! issued episode — the maximum across the journal's committed
//! envelope identities **and** its commit-carried frontier entries
//! (canceled transactions included — a durably issued identity is
//! never reused), the chain-confirmed identities, and the anchor's
//! snapshot identities — never inferred from the latest snapshot
//! alone.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use engine_core::SaveError;

use crate::consequence::{AwardState, ConsequenceId, Coverage, OccurrenceSpan};
use crate::envelope::{
    ClassifiedSpan, ConsequenceEnvelope, CounterDelta, CounterValues, DurabilityClass,
    PartialApplication, SignalLane, SourcePosition,
};
use crate::hash::Hash;
use crate::journal::{JournalScan, TransactionId};

/// The verified recovery anchor: the latest confirmed snapshot's
/// authoritative consequential state (the generic B3 form — B4 maps
/// the NpcWorld's per-NPC counters and award high-watermarks onto it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryAnchor {
    /// The anchor's chain position and campaign tick (the temporal
    /// discriminator's anchor side).
    pub position: SourcePosition,
    /// Per-identity authoritative counters at the anchor.
    pub counters: BTreeMap<ConsequenceId, CounterValues>,
    /// Per-identity award state at the anchor — the **durable** lane
    /// is the materialized-coverage suppression authority; the
    /// provisional lane is encounter-governed (the application's
    /// boundary policy, outside recovery's scope — recovery only
    /// clears provisional occurrences that its durable promotions
    /// absorb, keeping the lanes disjoint).
    pub awards: BTreeMap<ConsequenceId, AwardState>,
    /// Per identity, the span-less movements whose effects this
    /// snapshot MATERIALIZED, by exact producing transaction — the
    /// identity-exact materialization evidence, published atomically
    /// WITH the effects in the one snapshot publication (never a
    /// counter comparison, never a separate durable applied-marker
    /// log). A movement inserts no coverage span, so this is the
    /// only snapshot-side proof that its effect is already
    /// represented — the W6 suppression authority for movements.
    pub movements: BTreeMap<ConsequenceId, BTreeSet<TransactionId>>,
}

impl RecoveryAnchor {
    /// An empty anchor at `position`.
    pub fn new(position: SourcePosition) -> Self {
        RecoveryAnchor {
            position,
            counters: BTreeMap::new(),
            awards: BTreeMap::new(),
            movements: BTreeMap::new(),
        }
    }

    /// Attaches one identity's snapshot state (builder form).
    pub fn with_identity(
        mut self,
        id: ConsequenceId,
        counters: CounterValues,
        award: AwardState,
    ) -> Self {
        self.counters.insert(id, counters);
        self.awards.insert(id, award);
        self
    }

    /// Attaches one identity's materialized-movement evidence (the
    /// producing transactions whose span-less effects the snapshot
    /// contains — carried into the next anchor's construction
    /// atomically with the recovered counters).
    pub fn with_materialized_movement(
        mut self,
        id: ConsequenceId,
        transactions: impl IntoIterator<Item = TransactionId>,
    ) -> Self {
        self.movements.entry(id).or_default().extend(transactions);
        self
    }

    fn counters(&self, id: ConsequenceId) -> CounterValues {
        self.counters.get(&id).copied().unwrap_or_default()
    }

    fn durable(&self, id: ConsequenceId) -> Coverage {
        self.awards
            .get(&id)
            .map(|award| award.durable.clone())
            .unwrap_or_default()
    }

    fn provisional(&self, id: ConsequenceId) -> Coverage {
        self.awards
            .get(&id)
            .map(|award| award.provisional.clone())
            .unwrap_or_default()
    }

    fn materialized_movements(&self, id: ConsequenceId) -> BTreeSet<TransactionId> {
        self.movements.get(&id).cloned().unwrap_or_default()
    }
}

/// One chain-confirmed identity's canonical evidence — the exact
/// occurrence coverage, producing transaction, and effect commitment
/// the verified chain carries (the pipeline's per-key view via
/// [`crate::pipeline::CommitPipeline::confirmed_evidence`], or any
/// caller with equivalent verified provenance). A bare identity set
/// is insufficient: the coherence and provenance checks must compare
/// the canonical entry's coverage, transaction, and effect commitment
/// against the anchor's materialization and the journal's envelopes —
/// identity-level equality is content-blind by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedEvidence {
    /// The confirmed occurrence coverage (empty for a span-less
    /// movement — the transaction is then the evidence).
    pub coverage: Vec<ClassifiedSpan>,
    /// The producing transaction the canonical batch installed.
    pub transaction: TransactionId,
    /// The producing envelope's effect commitment (the SHA-256 of
    /// its canonical bytes) — the semantic binding distinguishing
    /// effect equivalence from identity-level equality: two valid
    /// payloads sharing the identity, span, classification, and plan
    /// (and therefore the TransactionId) but feeding different
    /// signal lanes carry different commitments.
    pub effect: Hash,
}

/// Contract B's classification of one transaction against the anchor
/// (§5.5.2's three cases plus the benign older-anchor path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionClassification {
    /// The anchor predates the transaction's source position — the
    /// absence of transient state is expected and benign: Contract A
    /// proceeds unconditionally; the transition is
    /// consequence-recovered, **transition not reconstructed** (the
    /// encounter-boundary path — B4 owns the exact reconstruction).
    BenignAbsence,
    /// The anchor is at-or-after the source and materializes none of
    /// the envelope's occurrences (the counters corroborate) — the
    /// transition applies cleanly.
    PreState,
    /// The anchor materializes part of the envelope's occurrences —
    /// neither wholly applied nor wholly absent; exactly the missing
    /// occurrences apply.
    PartialState,
    /// The anchor materializes every occurrence — nothing applies, no
    /// duplicate.
    PostState,
    /// The anchor's counters contradict what its coverage
    /// materializes (behind the coverage, or claiming the full
    /// post-state over partial coverage) — divergence suspicion:
    /// **fail closed, evidence preserved**.
    Incompatible { detail: &'static str },
}

impl fmt::Display for TransitionClassification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransitionClassification::BenignAbsence => write!(
                f,
                "consequence-recovered, transition not reconstructed (benign absence)"
            ),
            TransitionClassification::PreState => write!(f, "pre-state (applies cleanly)"),
            TransitionClassification::PartialState => {
                write!(f, "partial state (exactly the missing occurrences apply)")
            }
            TransitionClassification::PostState => write!(f, "post-state (no duplicate)"),
            TransitionClassification::Incompatible { detail } => {
                write!(
                    f,
                    "incompatible ({detail}) — fail closed, evidence preserved"
                )
            }
        }
    }
}

/// One applied effect (Contract A's output, in journal sequence
/// order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEffect {
    pub id: ConsequenceId,
    pub transaction: TransactionId,
    /// The occurrences applied (the missing sub-spans, with their
    /// classes).
    pub applied: Vec<ClassifiedSpan>,
    /// The exact counter delta applied (the uniform unit × the
    /// missing count — never proportional guesswork).
    pub delta: CounterDelta,
}

/// The recovery report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryReport {
    /// Contract A's applications, in journal sequence order.
    pub applied: Vec<AppliedEffect>,
    /// Contract B's per-transaction classification.
    pub transitions: Vec<(TransactionId, TransitionClassification)>,
    /// Identities suppressed by verified canonical inclusion (tier 1)
    /// — never re-applied. The canonical-state coherence contract
    /// gates this: a confirmed identity is suppressed only when the
    /// anchor (or the working coverage) MATERIALIZES its effects;
    /// a confirmed identity absent from the loaded state is
    /// recovered into `confirmed_recovered` instead — never
    /// silently discarded on batch identity alone.
    pub chain_suppressed: Vec<ConsequenceId>,
    /// Confirmed identities whose effects were ABSENT from the
    /// loaded state, recovered from the canonical evidence (the
    /// journal envelope under the chain's commitment) — exactly-once
    /// application, distinctly reported.
    pub confirmed_recovered: Vec<AppliedEffect>,
    /// Per identity, the occurrences already materialized in the
    /// anchor's (or the working) coverage — suppressed without
    /// duplication (tiers 3–4).
    pub materialized: Vec<(ConsequenceId, u64)>,
    /// Movements suppressed by the snapshot's identity-exact
    /// materialization evidence (the anchor's per-identity
    /// transaction set) — the W6 window: a published movement is
    /// never re-applied.
    pub movement_materialized: Vec<(ConsequenceId, TransactionId)>,
    /// Transactions suppressed by their durable cancellation (the
    /// terminated-encounter record) — their effects never apply.
    pub canceled: Vec<TransactionId>,
}

/// One identity's recovered authoritative state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredParticipant {
    pub counters: CounterValues,
    pub award: AwardState,
    /// The identity's materialized-movement set after this recovery:
    /// the anchor's inherited evidence ∪ the movements this run
    /// applied. The caller serializes it WITH the counters and the
    /// award lanes in the ONE snapshot publication — the evidence is
    /// atomically published with the effect, so the next recovery's
    /// W6 window cannot re-apply a movement the snapshot already
    /// contains.
    pub movements: BTreeSet<TransactionId>,
}

/// The reconciliation result: the recovered state for every identity
/// an actionable envelope touched (the anchor's entry — or the
/// default — with the missing occurrences promoted into the durable
/// lane and the counters advanced by the exact units), plus the
/// report. Unaffected identities pass through unchanged (the caller
/// merges).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryOutcome {
    pub recovered: BTreeMap<ConsequenceId, RecoveredParticipant>,
    pub report: RecoveryReport,
}

/// Typed recovery failures.
#[derive(Debug)]
pub enum RecoveryError {
    Format(SaveError),
    /// Contract B's incompatible case — the counters contradict the
    /// materialized coverage. Fail closed; the evidence (the anchor,
    /// the journal, the classification) is preserved for governed
    /// resolution.
    Incompatible {
        transaction: TransactionId,
        detail: &'static str,
    },
    /// The conservation invariant broke during application — a
    /// genuine inconsistency, never repaired.
    ConservationViolation {
        id: ConsequenceId,
    },
    /// The canonical-state coherence contract's fail-closed edge: a
    /// chain-confirmed identity whose effects are neither
    /// materialized in the loaded anchor nor recoverable from an
    /// actionable journal envelope — an unaccountable consequence.
    /// Never silently discarded, never fabricated: fail closed with
    /// the evidence preserved (the confirmed set, the anchor, and
    /// the journal scan are the caller's inputs — the resolution is
    /// governed, outside the pure function).
    ConfirmedUnmaterialized {
        id: ConsequenceId,
    },
    /// The journal envelope's provenance conflicts with the confirmed
    /// canonical entry — a different producing transaction or
    /// occurrence coverage for the same identity. Incompatible
    /// journal evidence is never a recovery source: fail closed,
    /// never a guess about which claim is authoritative.
    ConfirmedProvenanceMismatch {
        id: ConsequenceId,
    },
    /// Distinct actionable journal transactions claim the same
    /// occurrences of one identity with divergent authoritative
    /// effects — a different durability class or uniform signal lane
    /// for the same occurrence. The working coverage's
    /// occurrence-exact suppression is not semantic agreement: a
    /// conflicting claim is never silently deduplicated — fail
    /// closed with the evidence preserved, never a guess about
    /// which transaction is authoritative.
    ConflictingJournalEffects {
        id: ConsequenceId,
    },
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecoveryError::Format(error) => write!(f, "recovery format failure: {error}"),
            RecoveryError::Incompatible {
                transaction,
                detail,
            } => write!(
                f,
                "transition {transaction:?} is incompatible ({detail}) — \
                 fail closed, evidence preserved"
            ),
            RecoveryError::ConservationViolation { id } => write!(
                f,
                "the conservation invariant broke recovering {id} — \
                 never repaired"
            ),
            RecoveryError::ConfirmedUnmaterialized { id } => write!(
                f,
                "the chain-confirmed identity {id} is absent from the loaded \
                 state with no recoverable journal envelope — fail closed, \
                 never silently discarded"
            ),
            RecoveryError::ConfirmedProvenanceMismatch { id } => write!(
                f,
                "the journal evidence for the chain-confirmed identity {id} \
                 conflicts with the canonical entry — incompatible \
                 provenance fails closed"
            ),
            RecoveryError::ConflictingJournalEffects { id } => write!(
                f,
                "distinct journal transactions claim the occurrences of {id:?} \
                 with divergent authoritative effects — fail closed, never \
                 silently deduplicated"
            ),
        }
    }
}

impl std::error::Error for RecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RecoveryError::Format(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SaveError> for RecoveryError {
    fn from(error: SaveError) -> Self {
        RecoveryError::Format(error)
    }
}

/// One in-run applied unit — the per-occurrence semantic
/// contribution (occurrence span, durability class, uniform signal
/// lane) one journal envelope's application actually made, with its
/// producing transaction. The tier-4 semantic record: occurrences a
/// later actionable envelope re-claims are compared against it
/// BEFORE the working coverage suppresses the replay — the earlier
/// and later transactions must AGREE about the re-claimed
/// occurrences' authoritative effects (a divergent class or signal
/// lane is [`RecoveryError::ConflictingJournalEffects`], never a
/// silent deduplication); a same-unit re-claim stays exactly-once
/// (the legitimate duplicate intent). In-run state only — it dies
/// with the process and re-derives identically on restart.
struct AppliedUnit {
    span: OccurrenceSpan,
    class: DurabilityClass,
    lane: SignalLane,
    transaction: TransactionId,
}

/// The (anchor, journal, confirmed-chain) reconciliation — Contract A
/// unconditional, Contract B classified, the four-tier suppression
/// exact. **A pure function**: repeated calls derive identical
/// results; no durable intermediate claims representation (the
/// recovered state becomes durable only at the caller's next
/// snapshot/revision publication — the W4 property).
///
/// The journal is scanned in sequence order; `confirmed` is the
/// chain-confirmed canonical evidence per identity (the pipeline's
/// `confirmed_evidence()` — verified inclusion WITH the exact
/// coverage and producing transaction, never the journal's mere
/// presence; a bare identity set cannot validate provenance). A
/// confirmed identity with no journal envelope is recognized through
/// its canonical provenance plus exact snapshot materialization —
/// the historical T1 journal is not required for an
/// already-represented consequence. Distinct actionable
/// transactions re-claiming the SAME occurrences must AGREE about
/// those occurrences' authoritative effects — the durability class
/// and uniform signal lane, compared per-unit before the working
/// coverage suppresses the replay: a divergent claim fails closed
/// ([`RecoveryError::ConflictingJournalEffects`]), a same-unit
/// re-claim stays exactly-once.
pub fn reconcile(
    anchor: &RecoveryAnchor,
    journal: &JournalScan,
    confirmed: &BTreeMap<ConsequenceId, ConfirmedEvidence>,
) -> Result<RecoveryOutcome, RecoveryError> {
    let canceled: BTreeSet<TransactionId> = journal
        .cancellations
        .iter()
        .map(|cancellation| cancellation.target)
        .collect();
    let mut working: BTreeMap<ConsequenceId, RecoveredParticipant> = BTreeMap::new();
    let mut report = RecoveryReport::default();
    // The tier-4 semantic record — per identity, the units this run
    // has actually applied (span, class, lane, producing
    // transaction). In-run only; dies with the process.
    let mut applied_units: BTreeMap<ConsequenceId, Vec<AppliedUnit>> = BTreeMap::new();

    for transaction in &journal.committed {
        if canceled.contains(&transaction.id) {
            // The durable termination record: a terminated
            // encounter's journaled intents never promote.
            report.canceled.push(transaction.id);
            continue;
        }
        let mut transaction_classification = TransitionClassification::BenignAbsence;
        let mut classified_any = false;
        for envelope in &transaction.envelopes {
            // The working participant — hoisted: every identity an
            // actionable envelope touches passes through (suppressed
            // or recovered), so the caller's merge carries the
            // movement evidence either way.
            let participant = working
                .entry(envelope.id)
                .or_insert_with(|| RecoveredParticipant {
                    counters: anchor.counters(envelope.id),
                    award: anchor.awards.get(&envelope.id).cloned().unwrap_or_default(),
                    movements: anchor.materialized_movements(envelope.id),
                });
            let span_bearing = !envelope.sub_spans.is_empty();
            // The partial application against the WORKING coverage
            // (the anchor's materialized coverage ∪ this run's
            // applications: the in-run dedup, which dies with the
            // process and re-derives identically).
            let application = if span_bearing {
                Some(envelope.missing_against(&participant.award.durable)?)
            } else {
                None
            };
            // Tier 1 under the canonical-state coherence contract:
            // chain inclusion is a COMMITMENT statement, never a
            // loaded-state statement — and the journal envelope must
            // carry EXACTLY the canonical provenance (the same
            // producing transaction and occurrence coverage; a
            // mismatch is incompatible evidence, never a recovery
            // source). A confirmed identity whose effects the anchor
            // materializes is fully represented (W7 — no action); a
            // confirmed identity ABSENT from the loaded state falls
            // through and is recovered from the canonical evidence
            // below — never silently discarded on batch identity
            // alone.
            let confirmed_evidence = confirmed.get(&envelope.id);
            if let Some(evidence) = confirmed_evidence {
                if evidence.transaction != transaction.id
                    || evidence.coverage != envelope.sub_spans
                    || evidence.effect
                        != envelope
                            .effect_commitment()
                            .map_err(RecoveryError::Format)?
                {
                    return Err(RecoveryError::ConfirmedProvenanceMismatch { id: envelope.id });
                }
                let materialized = match &application {
                    Some(application) => application.is_empty(),
                    None => participant.movements.contains(&transaction.id),
                };
                if materialized {
                    report.chain_suppressed.push(envelope.id);
                    if !span_bearing {
                        report
                            .movement_materialized
                            .push((envelope.id, transaction.id));
                    }
                    continue;
                }
            } else {
                // Contract B — unconfirmed envelopes only (a
                // confirmed identity's transition is canonically
                // settled; its recovery applies the evidence forward).
                let classification = classify_transition(anchor, envelope)?;
                if let TransitionClassification::Incompatible { detail } = classification {
                    return Err(RecoveryError::Incompatible {
                        transaction: transaction.id,
                        detail,
                    });
                }
                if !classified_any {
                    transaction_classification = classification.clone();
                    classified_any = true;
                }
            }
            // A span-less lane movement (an activation commit):
            // identity-governed whole-or-not — the coverage cannot
            // suppress a movement, and counter equality is never
            // suppression evidence (unrelated operations can produce
            // identical values). The durable suppression authorities
            // are the chain (tier 1) and the snapshot's identity-exact
            // materialization evidence (checked above — the W6
            // window); each journal transaction applies exactly once
            // per run, and the application records its transaction
            // into the recovered movement set so the caller's next
            // snapshot publication carries the evidence atomically
            // with the effect.
            if !span_bearing {
                // Tier 3 for movements — the snapshot's identity-exact
                // materialization evidence (the W6 window): a movement
                // the published snapshot already contains is never
                // reapplied, chain-confirmed or not (chain
                // confirmation is routinely absent in exactly this
                // window).
                if participant.movements.contains(&transaction.id) {
                    report
                        .movement_materialized
                        .push((envelope.id, transaction.id));
                    continue;
                }
                let delta = envelope.delta;
                participant.counters = delta.apply_to(&participant.counters)?;
                participant.movements.insert(transaction.id);
                if !participant.counters.conservation_holds() {
                    return Err(RecoveryError::ConservationViolation { id: envelope.id });
                }
                let effect = AppliedEffect {
                    id: envelope.id,
                    transaction: transaction.id,
                    applied: Vec::new(),
                    delta,
                };
                if confirmed_evidence.is_some() {
                    report.confirmed_recovered.push(effect);
                } else {
                    report.applied.push(effect);
                }
                continue;
            }
            let application = application.expect("span-bearing envelopes computed it above");
            // The tier-4 semantic gate (the fourth re-review's
            // finding): the working coverage's suppression is
            // occurrence-exact but never semantically blind — a
            // later actionable envelope re-claiming occurrences an
            // earlier DIFFERENT journal transaction applied is
            // compared per-unit BEFORE the coverage suppresses the
            // replay. The two transactions must AGREE about the
            // re-claimed occurrences' authoritative effects (the
            // durability class and the uniform signal lane): a
            // divergent claim (+2 pending vs +2 captured for the
            // same occurrences) is a conflict — fail closed with
            // typed evidence, never silently deduplicated; a
            // same-unit re-claim (the legitimate duplicate intent —
            // partial overlaps and holes included) stays
            // exactly-once.
            let lane = envelope
                .signal_lane()
                .map_err(RecoveryError::Format)?
                .ok_or(SaveError::Integrity("envelope/uniform-unit-violation"))
                .map_err(RecoveryError::Format)?;
            for unit in applied_units.get(&envelope.id).into_iter().flatten() {
                if unit.transaction == transaction.id {
                    // One identity is one envelope per journal
                    // transaction (the append-side identity-set
                    // validation); a same-transaction re-claim is
                    // the same intent, never a conflict.
                    continue;
                }
                for classified in &envelope.sub_spans {
                    let lo = unit.span.lo.max(classified.span.lo);
                    let hi = unit.span.hi.min(classified.span.hi);
                    if lo < hi && (unit.class != classified.class || unit.lane != lane) {
                        return Err(RecoveryError::ConflictingJournalEffects { id: envelope.id });
                    }
                }
            }
            if application.missing.is_empty() {
                report
                    .materialized
                    .push((envelope.id, envelope.occurrences()?));
                continue;
            }
            let delta = total_delta(&application)?;
            participant.counters = delta.apply_to(&participant.counters)?;
            for classified in &application.missing {
                // The promote semantics: the recovered occurrences
                // enter the durable lane and leave the provisional
                // lane — cross-lane coherence preserved.
                participant.award.durable.insert_span(classified.span);
                participant.award.provisional.remove_span(&classified.span);
                applied_units
                    .entry(envelope.id)
                    .or_default()
                    .push(AppliedUnit {
                        span: classified.span,
                        class: classified.class,
                        lane,
                        transaction: transaction.id,
                    });
            }
            if !participant.counters.conservation_holds() {
                return Err(RecoveryError::ConservationViolation { id: envelope.id });
            }
            let effect = AppliedEffect {
                id: envelope.id,
                transaction: transaction.id,
                applied: application.missing,
                delta,
            };
            if confirmed_evidence.is_some() {
                report.confirmed_recovered.push(effect);
            } else {
                report.applied.push(effect);
            }
        }
        if classified_any {
            report
                .transitions
                .push((transaction.id, transaction_classification));
        }
    }
    // The canonical-state coherence contract's closing edge: every
    // chain-confirmed identity must be accountable — recovered from
    // an actionable journal envelope (the loop above) or MATERIALIZED
    // in the verified anchor. A confirmed identity with no journal
    // envelope is recognized through its canonical provenance plus
    // exact snapshot materialization (the chain-verified coverage
    // against the anchor's durable lane; for a movement, the anchor's
    // identity-exact transaction evidence) — the historical T1
    // journal is NOT required for an already-represented consequence.
    // A confirmed identity that is neither materialized nor
    // recoverable is an unaccountable consequence: fail closed,
    // never silently discarded, never fabricated.
    for (identity, evidence) in confirmed {
        if working.contains_key(identity) {
            continue;
        }
        let materialized = if evidence.coverage.is_empty() {
            anchor
                .materialized_movements(*identity)
                .contains(&evidence.transaction)
        } else {
            let durable = anchor.durable(*identity);
            evidence
                .coverage
                .iter()
                .all(|classified| durable.missing_of(&classified.span).is_empty())
        };
        if !materialized {
            return Err(RecoveryError::ConfirmedUnmaterialized { id: *identity });
        }
        report.chain_suppressed.push(*identity);
        if evidence.coverage.is_empty() {
            report
                .movement_materialized
                .push((*identity, evidence.transaction));
        }
    }
    Ok(RecoveryOutcome {
        recovered: working,
        report,
    })
}

/// Contract B's classification of one envelope against the anchor
/// (pure; the temporal discriminator first, then the comparable-state
/// check).
fn classify_transition(
    anchor: &RecoveryAnchor,
    envelope: &ConsequenceEnvelope,
) -> Result<TransitionClassification, RecoveryError> {
    // The temporal discriminator: the anchor predates the source ⇒
    // benign absence (Contract A unconditional; the transition is
    // not reconstructed).
    if anchor.position.tick <= envelope.position.tick {
        return Ok(TransitionClassification::BenignAbsence);
    }
    let total = envelope.occurrences()?;
    if total == 0 {
        // A span-less lane movement (an activation): identity-governed
        // whole-or-not — the chain (tier 1) is the only durable
        // suppression authority. Counter equality is never evidence
        // of an applied identity: unrelated operations can produce
        // identical values, and two journaled movements with
        // identical deltas must each apply exactly once.
        return Ok(TransitionClassification::PreState);
    }
    let anchor_durable = anchor.durable(envelope.id);
    let application = envelope.missing_against(&anchor_durable)?;
    let missing: u64 = application
        .missing
        .iter()
        .map(|classified| classified.span.hi - classified.span.lo)
        .sum();
    let covered = total - missing;
    // The comparable-state check. Individual signal lanes are NOT
    // monotone: valid captures transfer pending → captured and
    // activations transfer captured → credits, so a later, causally
    // valid anchor may hold LESS of an individual lane than the
    // envelope's own units added — no individual lane is ever
    // compared. The witnesses are the milestone counter (awards only
    // add, per identity) and the occurrence coverage (per identity,
    // either lane): a coherent at-or-after anchor holds exactly the
    // envelope's baseline milestones plus the newly-materialized
    // occurrences' units — fewer milestones than the coverage
    // materializes, or more milestones than any lane explains, are
    // contradictions: fail closed.
    let anchor_milestones = anchor.counters(envelope.id).milestones;
    let baseline_milestones = envelope.baseline.counters.milestones;
    let newly = newly_materialized(anchor, envelope);
    let explained = baseline_milestones.saturating_add(newly);
    if anchor_milestones < explained {
        return Ok(TransitionClassification::Incompatible {
            detail: "milestones-behind-materialized-coverage",
        });
    }
    if anchor_milestones > explained {
        return Ok(TransitionClassification::Incompatible {
            detail: "milestones-claim-unmaterialized-coverage",
        });
    }
    Ok(if covered == 0 {
        TransitionClassification::PreState
    } else if covered < total {
        TransitionClassification::PartialState
    } else {
        TransitionClassification::PostState
    })
}

/// The occurrences of `span` a single lane covers.
fn lane_covered_count(lane: &Coverage, span: &OccurrenceSpan) -> u64 {
    let total = span.hi - span.lo;
    let missing: u64 = lane
        .missing_of(span)
        .iter()
        .map(|missing| missing.hi - missing.lo)
        .sum();
    total - missing
}

/// The occurrences of the envelope's span materialized since
/// preparation: covered by the anchor's award lanes (either lane —
/// the mid-encounter provisional state is legitimate) minus the
/// baseline's pre-existing coverage (saturating — a rolled-back
/// provisional restores coherently below the baseline's union).
fn newly_materialized(anchor: &RecoveryAnchor, envelope: &ConsequenceEnvelope) -> u64 {
    let mut newly: u64 = 0;
    for classified in &envelope.sub_spans {
        let span = classified.span;
        let anchor_covered = lane_covered_count(&anchor.durable(envelope.id), &span)
            + lane_covered_count(&anchor.provisional(envelope.id), &span);
        let baseline_covered = lane_covered_count(&envelope.baseline.award.durable, &span)
            + lane_covered_count(&envelope.baseline.award.provisional, &span);
        newly += anchor_covered.saturating_sub(baseline_covered);
    }
    newly
}

/// The total application delta: the durable and provisional parts
/// summed (both classes of an actionable transaction's missing
/// occurrences — the commit evidence covers the encounter's
/// acceptance; checked arithmetic).
fn total_delta(application: &PartialApplication) -> Result<CounterDelta, SaveError> {
    let sum = |a: i64, b: i64| {
        a.checked_add(b)
            .ok_or(SaveError::Integrity("recovery/delta-arithmetic-overflow"))
    };
    Ok(CounterDelta {
        milestones: application
            .durable_delta
            .milestones
            .checked_add(application.provisional_delta.milestones)
            .ok_or(SaveError::Integrity("recovery/delta-arithmetic-overflow"))?,
        pending_signals: sum(
            application.durable_delta.pending_signals,
            application.provisional_delta.pending_signals,
        )?,
        captured: sum(
            application.durable_delta.captured,
            application.provisional_delta.captured,
        )?,
        credits: sum(
            application.durable_delta.credits,
            application.provisional_delta.credits,
        )?,
    })
}

/// The monotonic identity-frontier reconstruction: the next fresh
/// episode for `region` advances beyond every durably issued one —
/// the maximum across the journal's committed envelope identities
/// (canceled transactions included — a durably issued identity is
/// never reused) **and** its commit-carried frontier entries, the
/// chain-confirmed identities, and the anchor's snapshot identities —
/// plus one (overflow fails closed). Never inferred from the latest
/// snapshot alone.
pub fn reconstruct_episode_frontier(
    region: u32,
    anchor: &RecoveryAnchor,
    journal: &JournalScan,
    confirmed: &BTreeSet<ConsequenceId>,
) -> Result<u64, RecoveryError> {
    let mut frontier: u64 = 0;
    let mut advance = |episode: u64| {
        frontier = frontier.max(episode);
    };
    for transaction in &journal.committed {
        for envelope in &transaction.envelopes {
            if envelope.id.region == region {
                advance(envelope.id.episode);
            }
        }
        for entry in &transaction.frontier {
            if entry.region == region {
                advance(entry.episode);
            }
        }
    }
    for id in confirmed {
        if id.region == region {
            advance(id.episode);
        }
    }
    for id in anchor.awards.keys() {
        if id.region == region {
            advance(id.episode);
        }
    }
    frontier
        .checked_add(1)
        .ok_or(RecoveryError::Format(SaveError::Integrity(
            "recovery/frontier-overflow",
        )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consequence::{AwardCheckpoint, OccurrenceSpan};
    use crate::envelope::{DurabilityClass, SignalLane, SourceBaseline};
    use crate::hash::Hash;
    use crate::journal::{CommittedTransaction, FrontierEntry};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("s7-r6-unit-{}-{label}-{seq}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test root");
            TestDir(path)
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

    fn unit(lane: SignalLane, count: u64) -> CounterValues {
        CounterDelta::uniform(lane, count)
            .unwrap()
            .apply_to(&CounterValues::default())
            .unwrap()
    }

    fn envelope(
        id: ConsequenceId,
        span: (u64, u64),
        class: DurabilityClass,
        tick: u64,
        baseline_counters: CounterValues,
    ) -> ConsequenceEnvelope {
        ConsequenceEnvelope::capture(
            id,
            vec![ClassifiedSpan {
                span: OccurrenceSpan::new(span.0, span.1).unwrap(),
                class,
            }],
            SignalLane::Captured,
            baseline_counters,
            SourceBaseline {
                counters: baseline_counters,
                award: AwardCheckpoint {
                    durable: Coverage::new(),
                    provisional: Coverage::new(),
                },
            },
            SourcePosition { head: None, tick },
        )
        .unwrap()
    }

    fn transaction(sequence: u64, envelopes: Vec<ConsequenceEnvelope>) -> CommittedTransaction {
        CommittedTransaction {
            id: TransactionId(Hash::from_bytes(&[sequence as u8; 32]).unwrap()),
            sequence,
            record_count: envelopes.len() as u64,
            digest: Hash::ZERO,
            envelopes,
            position: SourcePosition {
                head: None,
                tick: 0,
            },
            classification: DurabilityClass::DurableEligible,
            dependencies: Vec::new(),
            frontier: Vec::new(),
            plan: Vec::new(),
        }
    }

    fn scan(transactions: Vec<CommittedTransaction>) -> JournalScan {
        JournalScan {
            committed: transactions,
            cancellations: Vec::new(),
            tail: crate::journal::TailState::Clean,
        }
    }

    fn canceled_scan(transaction: CommittedTransaction) -> JournalScan {
        let target = transaction.id;
        JournalScan {
            committed: vec![transaction],
            cancellations: vec![crate::journal::RecordedCancellation {
                sequence: 2,
                target,
                reason: crate::journal::CancellationReason::ApplicationPolicy,
                detail: Vec::new(),
            }],
            tail: crate::journal::TailState::Clean,
        }
    }

    fn id(episode: u64) -> ConsequenceId {
        ConsequenceId::new(1, episode, 1)
    }

    // ------------------------------------------------------------ Contract A

    #[test]
    fn older_anchor_recovers_by_identity_and_missing_coverage() {
        // §5.4.1's walkthrough: the confirmed tick-100 anchor holds
        // none of the crossing's state; the envelope restores the
        // complete delta atomically — milestones AND the signal lane
        // together — while steps (structurally absent from the
        // schema) are never fabricated.
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let identity = id(1);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (0, 1),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(outcome.report.transitions.len(), 1);
        assert_eq!(
            outcome.report.transitions[0].1,
            TransitionClassification::BenignAbsence
        );
        assert_eq!(outcome.report.applied.len(), 1);
        assert_eq!(outcome.report.applied[0].delta.milestones, 1);
        assert_eq!(outcome.report.applied[0].delta.captured, 1);
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(participant.counters, unit(SignalLane::Captured, 1));
        assert!(participant.award.durable.covers(1));
        assert!(participant.counters.conservation_holds());
        // The exact arithmetic: the recovered counters are the
        // anchor's ⊕ the applied delta — nothing else (no fabricated
        // steps, no transient history).
        assert_eq!(
            participant.counters,
            outcome.report.applied[0]
                .delta
                .apply_to(&CounterValues::default())
                .unwrap()
        );
    }

    #[test]
    fn partially_present_aggregate_applies_exactly_the_missing() {
        // §5.4.1 test 4: an anchor containing occurrences 4–6 of the
        // aggregate's span (3,7] — only occurrence 7 applies; the
        // aggregate is neither wholly applied nor wholly absent.
        let identity = id(1);
        let mut award = AwardState::new();
        award.apply_durable_span(OccurrenceSpan::new(3, 6).unwrap());
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 200,
        })
        .with_identity(identity, unit(SignalLane::Captured, 3), award);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (3, 7),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(
            outcome.report.transitions[0].1,
            TransitionClassification::PartialState
        );
        assert_eq!(outcome.report.applied.len(), 1);
        assert_eq!(
            outcome.report.applied[0].delta.milestones, 1,
            "only occurrence 7"
        );
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(participant.counters, unit(SignalLane::Captured, 4));
        assert!(participant.award.durable.covers(7));
        assert!(!participant.award.durable.covers(8));
        assert!(participant.counters.conservation_holds());
    }

    #[test]
    fn hole_bearing_anchor_fills_the_hole_without_duplication() {
        // The reviewer's counterexample: an anchor covering {4, 6}
        // against the span (3,7] yields exactly the missing {5, 7} —
        // the legitimate hole fills, composing back to one span,
        // nothing double-applies.
        let identity = id(1);
        let mut award = AwardState::new();
        award.apply_durable_span(OccurrenceSpan::new(3, 4).unwrap());
        award.apply_durable_span(OccurrenceSpan::new(5, 6).unwrap());
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 200,
        })
        .with_identity(identity, unit(SignalLane::Captured, 2), award);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (3, 7),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(
            outcome.report.applied[0].delta.milestones, 2,
            "occurrences 5 and 7"
        );
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(participant.counters, unit(SignalLane::Captured, 4));
        // The hole filled and composed back to one span.
        assert_eq!(participant.award.durable.spans().len(), 1);
        assert_eq!(participant.award.durable.spans()[0].lo, 3);
        assert_eq!(participant.award.durable.spans()[0].hi, 7);
    }

    #[test]
    fn fully_materialized_transition_applies_nothing() {
        // The durable-award-no-dup case: the anchor materializes
        // every occurrence — nothing applies, no duplicate.
        let identity = id(1);
        let mut award = AwardState::new();
        award.apply_durable_span(OccurrenceSpan::new(3, 7).unwrap());
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 200,
        })
        .with_identity(identity, unit(SignalLane::Captured, 4), award);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (3, 7),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert!(outcome.report.applied.is_empty());
        assert_eq!(
            outcome.report.transitions[0].1,
            TransitionClassification::PostState
        );
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(
            participant.counters,
            unit(SignalLane::Captured, 4),
            "no duplicate"
        );
    }

    #[test]
    fn confirmed_and_materialized_suppresses_w7() {
        // The canonical-state coherence contract, the represented
        // side: a chain-confirmed identity whose effects the anchor
        // MATERIALIZES is fully represented (W7) — the chain tier
        // suppresses, nothing applies, and the pass-through state is
        // unchanged.
        let identity = id(1);
        let mut award = AwardState::new();
        award.award_durable(1);
        award.award_durable(2);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        })
        .with_identity(identity, unit(SignalLane::Captured, 2), award);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (0, 2),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let mut confirmed = BTreeMap::new();
        confirmed.insert(
            identity,
            ConfirmedEvidence {
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction: journal.committed[0].id,
                effect: journal.committed[0].envelopes[0]
                    .effect_commitment()
                    .unwrap(),
            },
        );
        let outcome = reconcile(&anchor, &journal, &confirmed).unwrap();
        assert_eq!(outcome.report.chain_suppressed, vec![identity]);
        assert!(outcome.report.applied.is_empty());
        assert!(outcome.report.confirmed_recovered.is_empty());
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(participant.counters, unit(SignalLane::Captured, 2));
    }

    #[test]
    fn confirmed_but_unmaterialized_recovers_from_canonical_evidence() {
        // The review's finding: an older anchor lacking a
        // chain-confirmed effect must NOT silently discard it —
        // chain inclusion is a commitment statement, never a
        // loaded-state statement. The effect is recovered from the
        // canonical evidence (the journal envelope) exactly once and
        // reported distinctly, never on batch identity alone.
        let identity = id(1);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (0, 2),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let mut confirmed = BTreeMap::new();
        confirmed.insert(
            identity,
            ConfirmedEvidence {
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction: journal.committed[0].id,
                effect: journal.committed[0].envelopes[0]
                    .effect_commitment()
                    .unwrap(),
            },
        );
        let outcome = reconcile(&anchor, &journal, &confirmed).unwrap();
        assert!(outcome.report.chain_suppressed.is_empty());
        assert!(outcome.report.applied.is_empty());
        assert_eq!(outcome.report.confirmed_recovered.len(), 1);
        assert_eq!(outcome.report.confirmed_recovered[0].id, identity);
        assert_eq!(
            outcome.report.confirmed_recovered[0].delta.milestones, 2,
            "the recovery reports the exact recovered delta"
        );
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(
            participant.counters,
            unit(SignalLane::Captured, 2),
            "the confirmed effect missing from the loaded state is recovered exactly once"
        );
        assert!(participant.counters.conservation_holds());
        assert!(participant.award.durable.covers(1) && participant.award.durable.covers(2));
    }

    #[test]
    fn confirmed_without_journal_evidence_fails_closed() {
        // The coherence contract's fail-closed edge: a chain-confirmed
        // identity whose effects are neither materialized in the
        // anchor nor recoverable from an actionable journal envelope
        // is an unaccountable consequence — never silently
        // discarded, never fabricated.
        let identity = id(1);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let journal = scan(vec![]);
        // The canonical evidence's transaction derives from the
        // historical attempt — the journal itself is absent, so no
        // envelope pairs with it; the empty anchor cannot materialize
        // the coverage either.
        let committed = transaction(
            1,
            vec![envelope(
                identity,
                (0, 2),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        );
        let transaction = committed.id;
        let effect = committed.envelopes[0].effect_commitment().unwrap();
        let mut confirmed = BTreeMap::new();
        confirmed.insert(
            identity,
            ConfirmedEvidence {
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction,
                effect,
            },
        );
        let error = reconcile(&anchor, &journal, &confirmed).unwrap_err();
        assert!(matches!(
            error,
            RecoveryError::ConfirmedUnmaterialized { id } if id == identity
        ));
    }

    #[test]
    fn confirmed_and_materialized_without_journal_succeeds() {
        // The re-review's finding: a verified anchor already
        // containing a confirmed consequence whose historical T1
        // journal is ABSENT — the canonical provenance plus exact
        // snapshot materialization recognize it as represented; the
        // journal is not required for an already-represented
        // consequence. Success, no replay, no error.
        let identity = id(1);
        let mut award = AwardState::new();
        award.award_durable(1);
        award.award_durable(2);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        })
        .with_identity(identity, unit(SignalLane::Captured, 2), award);
        let journal = scan(vec![]);
        let committed = transaction(
            1,
            vec![envelope(
                identity,
                (0, 2),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        );
        let transaction = committed.id;
        let effect = committed.envelopes[0].effect_commitment().unwrap();
        let mut confirmed = BTreeMap::new();
        confirmed.insert(
            identity,
            ConfirmedEvidence {
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction,
                effect,
            },
        );
        let outcome = reconcile(&anchor, &journal, &confirmed).unwrap();
        assert_eq!(outcome.report.chain_suppressed, vec![identity]);
        assert!(outcome.report.applied.is_empty());
        assert!(outcome.report.confirmed_recovered.is_empty());
        assert!(outcome.recovered.is_empty());
    }

    #[test]
    fn confirmed_with_conflicting_journal_provenance_fails_closed() {
        // The re-review's finding: the journal envelope's provenance
        // must match the confirmed canonical entry EXACTLY — a
        // different producing transaction or occurrence coverage is
        // incompatible evidence, never a recovery source: fail
        // closed, never a guess about which claim is authoritative.
        let identity = id(1);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (0, 2),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        // The canonical entry claims the same identity under a
        // DIFFERENT coverage — the journal's envelope conflicts.
        let mut confirmed = BTreeMap::new();
        confirmed.insert(
            identity,
            ConfirmedEvidence {
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(4, 6).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction: journal.committed[0].id,
                effect: journal.committed[0].envelopes[0]
                    .effect_commitment()
                    .unwrap(),
            },
        );
        let error = reconcile(&anchor, &journal, &confirmed).unwrap_err();
        assert!(matches!(
            error,
            RecoveryError::ConfirmedProvenanceMismatch { id } if id == identity
        ));
    }

    #[test]
    fn confirmed_with_conflicting_journal_effect_fails_closed() {
        // The third re-review's finding: `TransactionId` + occurrence
        // coverage is insufficient to prove effect equivalence — the
        // same identity, span, classification, and plan admits two
        // valid uniform lanes (+2 pending vs +2 captured). The
        // journal envelope's actual EFFECT COMMITMENT and the
        // confirmed chain's claimed effect must match exactly: a
        // canonical claim to a different consequential payload is
        // incompatible evidence, never a recovery source.
        let identity = id(1);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (0, 2),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        // The same identity, span, class, tick, and baseline — the
        // same TransactionId inputs — a different valid uniform
        // lane: a different effect commitment.
        let different_effect = ConsequenceEnvelope::capture(
            identity,
            vec![ClassifiedSpan {
                span: OccurrenceSpan::new(0, 2).unwrap(),
                class: DurabilityClass::DurableEligible,
            }],
            SignalLane::Pending,
            CounterValues::default(),
            SourceBaseline {
                counters: CounterValues::default(),
                award: AwardCheckpoint {
                    durable: Coverage::new(),
                    provisional: Coverage::new(),
                },
            },
            SourcePosition {
                head: None,
                tick: 110,
            },
        )
        .unwrap();
        // The canonical entry claims the journal's transaction and
        // coverage but the OTHER lane's effects.
        let mut confirmed = BTreeMap::new();
        confirmed.insert(
            identity,
            ConfirmedEvidence {
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction: journal.committed[0].id,
                effect: different_effect.effect_commitment().unwrap(),
            },
        );
        let error = reconcile(&anchor, &journal, &confirmed).unwrap_err();
        assert!(matches!(
            error,
            RecoveryError::ConfirmedProvenanceMismatch { id } if id == identity
        ));
    }

    #[test]
    fn movement_materialized_evidence_suppresses_w6_reapplication() {
        // The W6 window, the review's finding: a span-less movement
        // already materialized in a verified snapshot is NOT
        // reapplied when chain confirmation is absent. The
        // suppression evidence is identity-exact — the snapshot's
        // per-identity transaction set, atomically published with
        // the effect — never counter equality (the anchor's counters
        // here coincide with the post-state, and that alone never
        // suppresses).
        let identity = id(1);
        let movement = CounterDelta {
            milestones: 0,
            pending_signals: 0,
            captured: -2,
            credits: 2,
        };
        let prior = unit(SignalLane::Captured, 8);
        let build = || {
            ConsequenceEnvelope::new(identity)
                .with_delta(movement)
                .with_expected_prior(prior)
                .with_baseline(SourceBaseline {
                    counters: prior,
                    award: AwardCheckpoint {
                        durable: Coverage::new(),
                        provisional: Coverage::new(),
                    },
                })
                .with_position(SourcePosition {
                    head: None,
                    tick: 110,
                })
        };
        let journal = scan(vec![transaction(1, vec![build()])]);
        let transaction = journal.committed[0].id;
        // The first recovery (the publication's source): the movement
        // applies, and the recovered state carries its transaction as
        // the materialization evidence.
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        })
        .with_identity(identity, prior, AwardState::new());
        let first = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        let participant = first.recovered.get(&identity).unwrap();
        let published = movement.apply_to(&prior).unwrap();
        assert_eq!(participant.counters, published);
        assert_eq!(participant.movements, BTreeSet::from([transaction]));
        assert_eq!(first.report.applied.len(), 1);
        // The published snapshot: the post-state counters PLUS the
        // identity-exact evidence, atomically — the caller's ONE
        // publication. The W6 window: chain confirmation is absent,
        // yet the movement must not reapply.
        let published_anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 120,
        })
        .with_identity(identity, published, AwardState::new())
        .with_materialized_movement(identity, [transaction]);
        let second = reconcile(&published_anchor, &journal, &BTreeMap::new()).unwrap();
        assert!(second.report.applied.is_empty());
        assert_eq!(
            second.report.movement_materialized,
            vec![(identity, transaction)]
        );
        let participant = second.recovered.get(&identity).unwrap();
        assert_eq!(
            participant.counters, published,
            "the materialized movement is never reapplied in the W6 window"
        );
        assert!(participant.counters.conservation_holds());
        assert!(
            participant.movements.contains(&transaction),
            "the evidence survives the pass-through for the caller's next publication"
        );
        // Repeated recovery from the same published anchor derives
        // the identical result (the pure function).
        let third = reconcile(&published_anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(second, third);
    }

    #[test]
    fn canceled_transactions_never_promote() {
        // A terminated encounter's durable cancellation suppresses
        // its journaled intents — the provisional effects never
        // promote to permanent progress.
        let identity = id(1);
        let journal = canceled_scan(transaction(
            1,
            vec![envelope(
                identity,
                (0, 2),
                DurabilityClass::EncounterProvisional,
                110,
                CounterValues::default(),
            )],
        ));
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(outcome.report.canceled.len(), 1);
        assert!(outcome.report.applied.is_empty());
    }

    #[test]
    fn in_run_dedup_across_overlapping_envelopes() {
        // Two journal transactions with overlapping spans for one
        // identity: the first applies its occurrences, the second's
        // missing is computed against the WORKING coverage — exactly
        // the uncovered part, no duplicate (the in-run tier, which
        // dies with the process and re-derives identically).
        let identity = id(1);
        let journal = scan(vec![
            transaction(
                1,
                vec![envelope(
                    identity,
                    (0, 4),
                    DurabilityClass::DurableEligible,
                    110,
                    CounterValues::default(),
                )],
            ),
            transaction(
                2,
                vec![envelope(
                    identity,
                    (2, 6),
                    DurabilityClass::DurableEligible,
                    120,
                    CounterValues::default(),
                )],
            ),
        ]);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(outcome.report.applied.len(), 2);
        assert_eq!(
            outcome.report.applied[0].delta.milestones, 4,
            "occurrences 1-4"
        );
        assert_eq!(
            outcome.report.applied[1].delta.milestones, 2,
            "occurrences 5-6 only"
        );
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(participant.counters, unit(SignalLane::Captured, 6));
        assert!(participant.counters.conservation_holds());
    }

    #[test]
    fn same_unit_reclaim_across_transactions_stays_exactly_once() {
        // The compatible mirror of the conflict case: two distinct
        // transactions re-claiming the SAME occurrences under the
        // SAME unit (class + lane) — the later claim is the same
        // authoritative effect, a legitimate duplicate intent:
        // suppressed exactly-once, never a conflict.
        let identity = id(1);
        let journal = scan(vec![
            transaction(
                1,
                vec![envelope(
                    identity,
                    (0, 2),
                    DurabilityClass::DurableEligible,
                    110,
                    CounterValues::default(),
                )],
            ),
            transaction(
                2,
                vec![envelope(
                    identity,
                    (0, 2),
                    DurabilityClass::DurableEligible,
                    120,
                    CounterValues::default(),
                )],
            ),
        ]);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(outcome.report.applied.len(), 1, "the first applies");
        assert_eq!(
            outcome.report.materialized,
            vec![(identity, 2)],
            "the same-unit re-claim reports materialized, exactly-once"
        );
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(participant.counters, unit(SignalLane::Captured, 2));
        assert!(participant.counters.conservation_holds());
    }

    #[test]
    fn divergent_partial_overlap_fails_closed() {
        // The conflict check is per-occurrence, not whole-envelope:
        // a PARTIAL overlap under divergent lanes still conflicts on
        // the shared occurrences — the earlier transaction's claim
        // stands for them; the later's divergent claim is never
        // absorbed by the coverage suppression.
        let identity = id(1);
        let pending = ConsequenceEnvelope::capture(
            identity,
            vec![ClassifiedSpan {
                span: OccurrenceSpan::new(0, 2).unwrap(),
                class: DurabilityClass::DurableEligible,
            }],
            SignalLane::Pending,
            CounterValues::default(),
            SourceBaseline {
                counters: CounterValues::default(),
                award: AwardCheckpoint {
                    durable: Coverage::new(),
                    provisional: Coverage::new(),
                },
            },
            SourcePosition {
                head: None,
                tick: 110,
            },
        )
        .unwrap();
        let journal = scan(vec![
            transaction(1, vec![pending]),
            transaction(
                2,
                vec![envelope(
                    identity,
                    (0, 4),
                    DurabilityClass::DurableEligible,
                    120,
                    CounterValues::default(),
                )],
            ),
        ]);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let error = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap_err();
        assert!(matches!(
            error,
            RecoveryError::ConflictingJournalEffects { id } if id == identity
        ));
    }

    #[test]
    fn confirmed_provenance_gates_divergent_journal_claims() {
        // The composition with confirmed provenance: when one of the
        // two divergent transactions IS the canonically confirmed
        // one, the exact-provenance gate fires first — every journal
        // envelope of a confirmed identity is validated against the
        // canonical evidence, so the divergent second transaction
        // fails closed as incompatible provenance. The tier-4
        // semantic gate composes behind the confirmed gate, never
        // preempts it.
        let identity = id(1);
        let pending = ConsequenceEnvelope::capture(
            identity,
            vec![ClassifiedSpan {
                span: OccurrenceSpan::new(0, 2).unwrap(),
                class: DurabilityClass::DurableEligible,
            }],
            SignalLane::Pending,
            CounterValues::default(),
            SourceBaseline {
                counters: CounterValues::default(),
                award: AwardCheckpoint {
                    durable: Coverage::new(),
                    provisional: Coverage::new(),
                },
            },
            SourcePosition {
                head: None,
                tick: 120,
            },
        )
        .unwrap();
        let journal = scan(vec![
            transaction(
                1,
                vec![envelope(
                    identity,
                    (0, 2),
                    DurabilityClass::DurableEligible,
                    110,
                    CounterValues::default(),
                )],
            ),
            transaction(2, vec![pending]),
        ]);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let mut confirmed = BTreeMap::new();
        confirmed.insert(
            identity,
            ConfirmedEvidence {
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction: journal.committed[0].id,
                effect: journal.committed[0].envelopes[0]
                    .effect_commitment()
                    .unwrap(),
            },
        );
        let error = reconcile(&anchor, &journal, &confirmed).unwrap_err();
        assert!(matches!(
            error,
            RecoveryError::ConfirmedProvenanceMismatch { id } if id == identity
        ));
    }

    #[test]
    fn repeated_recovery_before_publication_is_idempotent() {
        // The W4/multi-crash property: reconcile is a pure function —
        // repeated calls (simulating crashes before the snapshot
        // publication) derive identical results; no durable
        // intermediate claims representation.
        let identity = id(1);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (0, 3),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let first = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        for _ in 0..3 {
            let again = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
            assert_eq!(again, first);
        }
        // The anchor itself is never mutated by the recovery.
        assert!(anchor.durable(identity).is_empty());
    }

    #[test]
    fn mixed_class_actionable_transaction_applies_the_total() {
        // An actionable (committed, uncanceled) transaction's commit
        // evidence covers the encounter's acceptance: both the
        // durable-classified and the provisional-classified missing
        // occurrences promote into the durable lane (a terminated
        // encounter either never journaled or was canceled — the
        // never-promote rule holds structurally).
        let identity = id(1);
        let journal = scan(vec![transaction(
            1,
            vec![
                ConsequenceEnvelope::capture(
                    identity,
                    vec![
                        ClassifiedSpan {
                            span: OccurrenceSpan::new(3, 4).unwrap(),
                            class: DurabilityClass::DurableEligible,
                        },
                        ClassifiedSpan {
                            span: OccurrenceSpan::new(4, 5).unwrap(),
                            class: DurabilityClass::EncounterProvisional,
                        },
                    ],
                    SignalLane::Captured,
                    CounterValues::default(),
                    SourceBaseline {
                        counters: CounterValues::default(),
                        award: AwardCheckpoint {
                            durable: Coverage::new(),
                            provisional: Coverage::new(),
                        },
                    },
                    SourcePosition {
                        head: None,
                        tick: 110,
                    },
                )
                .unwrap(),
            ],
        )]);
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(
            outcome.report.applied[0].delta.milestones, 2,
            "both classes"
        );
        let participant = outcome.recovered.get(&identity).unwrap();
        assert!(participant.award.durable.covers(4) && participant.award.durable.covers(5));
        assert!(participant.award.provisional.is_empty());
    }

    #[test]
    fn provisional_lane_is_cleared_by_durable_promotion() {
        // An anchor whose provisional lane holds an occurrence the
        // recovery promotes durably: the lanes stay disjoint (one
        // occurrence, two lanes, is invalid).
        let identity = id(1);
        let mut award = AwardState::new();
        award.apply_provisional_span(OccurrenceSpan::new(0, 2).unwrap());
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        })
        .with_identity(identity, CounterValues::default(), award);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (0, 2),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        let participant = outcome.recovered.get(&identity).unwrap();
        assert!(participant.award.durable.covers(1) && participant.award.durable.covers(2));
        assert!(
            participant.award.provisional.is_empty(),
            "cross-lane disjoint"
        );
        assert!(participant.award.validate().is_ok());
    }

    // ------------------------------------------------------------ Contract B

    #[test]
    fn counters_behind_materialized_coverage_fails_closed() {
        // The anchor's coverage claims 3 materialized occurrences but
        // its counters hold only 1 unit — the counters contradict the
        // coverage: fail closed, evidence preserved.
        let identity = id(1);
        let mut award = AwardState::new();
        award.apply_durable_span(OccurrenceSpan::new(3, 6).unwrap());
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 200,
        })
        .with_identity(identity, unit(SignalLane::Captured, 1), award);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (3, 7),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let error = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap_err();
        assert!(matches!(
            error,
            RecoveryError::Incompatible {
                detail: "milestones-behind-materialized-coverage",
                ..
            }
        ));
    }

    #[test]
    fn counters_claiming_unmaterialized_coverage_fail_closed() {
        // The anchor's counters equal the full post-state while its
        // coverage materializes only part — the contradiction fails
        // closed (a rolled-back hole discards its counter unit too;
        // counters claiming it are incoherent).
        let identity = id(1);
        let mut award = AwardState::new();
        award.apply_durable_span(OccurrenceSpan::new(3, 5).unwrap());
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 200,
        })
        .with_identity(identity, unit(SignalLane::Captured, 4), award);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (3, 7),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        let error = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap_err();
        assert!(matches!(
            error,
            RecoveryError::Incompatible {
                detail: "milestones-claim-unmaterialized-coverage",
                ..
            }
        ));
    }

    #[test]
    fn nonmonotonic_transfer_anchor_is_not_incompatible() {
        // The reviewer's regression: valid captures and activations
        // transfer values BETWEEN signal lanes, so a later, causally
        // valid anchor may hold LESS of an individual lane than the
        // envelope's own units added — the comparable-state check
        // must never consult an individual lane. The witnesses are
        // the milestone counter (awards only add, per identity) and
        // the occurrence coverage; conservation holds through every
        // transfer.
        let identity = id(1);
        let journal = scan(vec![transaction(
            1,
            vec![envelope(
                identity,
                (0, 4),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        )]);
        // The full variant: all four occurrences materialized, then a
        // legitimate activation transferred 3 captured -> credits —
        // captured holds 1 (LESS than the envelope's 4 units), the
        // milestones and the signal mass exact.
        let mut award = AwardState::new();
        award.apply_durable_span(OccurrenceSpan::new(0, 4).unwrap());
        let transferred = CounterValues {
            milestones: 4,
            captured: 1,
            credits: 3,
            ..CounterValues::default()
        };
        assert!(transferred.conservation_holds());
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 200,
        })
        .with_identity(identity, transferred, award);
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(
            outcome.report.transitions[0].1,
            TransitionClassification::PostState,
            "a causally valid transferred anchor is never incompatible"
        );
        assert!(outcome.report.applied.is_empty());

        // The partial variant: two occurrences materialized, then the
        // activation moved BOTH captured units to credits — the
        // missing occurrences 3-4 still apply exactly.
        let mut award = AwardState::new();
        award.apply_durable_span(OccurrenceSpan::new(0, 2).unwrap());
        let transferred = CounterValues {
            milestones: 2,
            credits: 2,
            ..CounterValues::default()
        };
        assert!(transferred.conservation_holds());
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 200,
        })
        .with_identity(identity, transferred, award);
        let outcome = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(
            outcome.report.transitions[0].1,
            TransitionClassification::PartialState
        );
        assert_eq!(
            outcome.report.applied[0].delta.milestones, 2,
            "occurrences 3-4"
        );
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(participant.counters.milestones, 4);
        assert!(participant.counters.conservation_holds());
    }

    #[test]
    fn movement_exact_once_despite_identical_counter_values() {
        // The reviewer's regression: the span-less movement's
        // suppression is IDENTITY-governed (the chain tier and the
        // in-run set) — counter equality is never evidence of an
        // applied identity. Two journaled movements with IDENTICAL
        // deltas (different transactions) against an anchor whose
        // counters coincidentally equal the first movement's
        // post-state (unrelated history): both apply, each exactly
        // once; repeated reconciliation (crashes before the snapshot
        // publication) derives the identical result — the recovered
        // state is anchor ⊕ the deltas, never anchor ⊕ deltas ×
        // runs.
        let identity = id(1);
        let movement = CounterDelta {
            milestones: 0,
            pending_signals: 0,
            captured: -2,
            credits: 2,
        };
        let baseline = unit(SignalLane::Captured, 8);
        let build = || {
            ConsequenceEnvelope::new(identity)
                .with_delta(movement)
                .with_expected_prior(baseline)
                .with_baseline(SourceBaseline {
                    counters: baseline,
                    award: AwardCheckpoint {
                        durable: Coverage::new(),
                        provisional: Coverage::new(),
                    },
                })
                .with_position(SourcePosition {
                    head: None,
                    tick: 110,
                })
        };
        let journal = scan(vec![
            transaction(1, vec![build()]),
            transaction(2, vec![build()]),
        ]);
        // The anchor's counters coincide with the first movement's
        // exact post-state — from unrelated history (the movements
        // never applied: chain-unconfirmed, nothing suppressed).
        let coincidental = movement.apply_to(&baseline).unwrap();
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 200,
        })
        .with_identity(identity, coincidental, AwardState::new());
        let first = reconcile(&anchor, &journal, &BTreeMap::new()).unwrap();
        assert_eq!(
            first.report.applied.len(),
            2,
            "both movements apply — neither suppressed by the other's counter effect"
        );
        let participant = first.recovered.get(&identity).unwrap();
        assert_eq!(
            participant.counters,
            CounterValues {
                milestones: 8,
                captured: 2,
                credits: 6,
                ..CounterValues::default()
            },
            "exactly two deltas from the coincidental anchor"
        );
        assert!(participant.counters.conservation_holds());
        // Repeated crashes before the snapshot publication: the pure
        // function re-derives the identical outcome.
        for _ in 0..3 {
            assert_eq!(
                reconcile(&anchor, &journal, &BTreeMap::new()).unwrap(),
                first
            );
        }
    }

    // ------------------------------------------------------------- frontier

    #[test]
    fn frontier_advances_beyond_every_durably_issued_episode() {
        // The max across the journal's envelope identities AND its
        // commit-carried frontier entries, the confirmed identities,
        // and the anchor's snapshot identities — plus one; never the
        // latest snapshot alone.
        let mut committed = transaction(
            1,
            vec![envelope(
                id(3),
                (0, 1),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            )],
        );
        committed.frontier = vec![FrontierEntry {
            region: 1,
            episode: 9,
        }];
        let journal = scan(vec![committed]);
        let mut confirmed = BTreeSet::new();
        confirmed.insert(id(5));
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        })
        .with_identity(id(2), CounterValues::default(), AwardState::new());
        assert_eq!(
            reconstruct_episode_frontier(1, &anchor, &journal, &confirmed).unwrap(),
            10,
            "the frontier entry's 9 wins"
        );
        // An untouched region starts at 1.
        assert_eq!(
            reconstruct_episode_frontier(7, &anchor, &journal, &confirmed).unwrap(),
            1
        );
        // Canceled transactions' identities are still durably issued
        // — never reused.
        let target = journal.committed[0].id;
        let canceled_scan = JournalScan {
            committed: journal.committed,
            cancellations: vec![crate::journal::RecordedCancellation {
                sequence: 2,
                target,
                reason: crate::journal::CancellationReason::ApplicationPolicy,
                detail: Vec::new(),
            }],
            tail: crate::journal::TailState::Clean,
        };
        assert_eq!(
            reconstruct_episode_frontier(1, &anchor, &canceled_scan, &confirmed).unwrap(),
            10,
            "the canceled transaction's episode 3 and frontier 9 still count"
        );
    }

    #[test]
    fn recovery_from_a_real_journal_scan() {
        // The end-to-end composition: a real T1Journal's scan feeds
        // the reconciliation (the generic fixtures stand in for the
        // B4 world state).
        let dir = TestDir::new("real-journal");
        let identity = id(1);
        {
            let mut journal = crate::journal::T1Journal::open(dir.journal()).unwrap();
            let envelope = envelope(
                identity,
                (0, 2),
                DurabilityClass::DurableEligible,
                110,
                CounterValues::default(),
            );
            let input = crate::journal::TransactionInput {
                envelopes: vec![envelope],
                position: SourcePosition {
                    head: None,
                    tick: 110,
                },
                classification: DurabilityClass::DurableEligible,
                dependencies: Vec::new(),
                frontier: vec![FrontierEntry {
                    region: 1,
                    episode: 1,
                }],
                plan: b"recovery-plan".to_vec(),
            };
            journal.append_transaction(&input).unwrap();
        }
        let scan = crate::journal::T1Journal::scan_file(dir.journal()).unwrap();
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let outcome = reconcile(&anchor, &scan, &BTreeMap::new()).unwrap();
        assert_eq!(outcome.report.applied.len(), 1);
        let participant = outcome.recovered.get(&identity).unwrap();
        assert_eq!(participant.counters, unit(SignalLane::Captured, 2));
        assert!(participant.counters.conservation_holds());
        assert_eq!(
            reconstruct_episode_frontier(1, &anchor, &scan, &BTreeSet::new()).unwrap(),
            2
        );
    }

    #[test]
    fn divergent_journal_effects_across_transactions_fail_closed() {
        // The fourth re-review's reproduction, on a REAL journal: two
        // distinct durably-committed transactions (distinct plans —
        // distinct TransactionIds) claiming the SAME identity and
        // span under DIFFERENT valid uniform lanes (+2 milestones/
        // +2 pending vs +2 milestones/+2 captured), neither
        // canonically confirmed, recovered from an older empty
        // anchor. The tier-4 dedup previously suppressed the second
        // claim silently — its missing set was empty, a successful
        // recovery despite conflicting authoritative effects; the
        // re-claimed occurrences are now compared per-unit BEFORE
        // the suppression: the divergent lane fails closed with
        // typed evidence.
        let dir = TestDir::new("divergent-lanes");
        let identity = id(1);
        {
            let mut journal = crate::journal::T1Journal::open(dir.journal()).unwrap();
            let pending = ConsequenceEnvelope::capture(
                identity,
                vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                SignalLane::Pending,
                CounterValues::default(),
                SourceBaseline {
                    counters: CounterValues::default(),
                    award: AwardCheckpoint {
                        durable: Coverage::new(),
                        provisional: Coverage::new(),
                    },
                },
                SourcePosition {
                    head: None,
                    tick: 110,
                },
            )
            .unwrap();
            journal
                .append_transaction(&crate::journal::TransactionInput {
                    envelopes: vec![pending],
                    position: SourcePosition {
                        head: None,
                        tick: 110,
                    },
                    classification: DurabilityClass::DurableEligible,
                    dependencies: Vec::new(),
                    frontier: vec![FrontierEntry {
                        region: 1,
                        episode: 1,
                    }],
                    plan: b"plan-pending".to_vec(),
                })
                .unwrap();
            let captured = ConsequenceEnvelope::capture(
                identity,
                vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                SignalLane::Captured,
                CounterValues::default(),
                SourceBaseline {
                    counters: CounterValues::default(),
                    award: AwardCheckpoint {
                        durable: Coverage::new(),
                        provisional: Coverage::new(),
                    },
                },
                SourcePosition {
                    head: None,
                    tick: 120,
                },
            )
            .unwrap();
            journal
                .append_transaction(&crate::journal::TransactionInput {
                    envelopes: vec![captured],
                    position: SourcePosition {
                        head: None,
                        tick: 120,
                    },
                    classification: DurabilityClass::DurableEligible,
                    dependencies: Vec::new(),
                    frontier: vec![FrontierEntry {
                        region: 1,
                        episode: 1,
                    }],
                    plan: b"plan-captured".to_vec(),
                })
                .unwrap();
        }
        let scan = crate::journal::T1Journal::scan_file(dir.journal()).unwrap();
        let anchor = RecoveryAnchor::new(SourcePosition {
            head: None,
            tick: 100,
        });
        let error = reconcile(&anchor, &scan, &BTreeMap::new()).unwrap_err();
        assert!(matches!(
            error,
            RecoveryError::ConflictingJournalEffects { id } if id == identity
        ));
    }
}
