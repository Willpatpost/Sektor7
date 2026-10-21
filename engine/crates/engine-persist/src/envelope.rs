//! The complete consequential-state envelope (`envelope/1`) — §5.4.1's
//! minimum sufficient journal record for **Contract A** (consequence
//! recovery: unconditional, by identity, from the journal alone).
//!
//! A real consequential event touches **multiple authoritative
//! counters atomically**: a milestone crossing carries
//! `{milestones[npc]: +1, signal → lane, coverage → occurrence}`
//! together. Recovering only half of a delta breaks the conservation
//! equation `milestones == credits + pending_signals + captured`; the
//! envelope therefore carries the **complete typed counter-delta** so
//! every affected authoritative counter restores together.
//! `steps` and routine-progression counters are **never
//! envelope-restorable** — they are actual-execution state, absent
//! from this schema by construction, never fabricated to repair
//! accounting (the milestone increment restored from an envelope is
//! not fabrication: it is the durable record of a real, verified
//! execution's consequence).
//!
//! **Class-homogeneous occurrence sub-spans** (the coverage-refinement
//! requirement): one aggregated `ConsequenceId` may cover occurrences
//! with different durability classifications, so the envelope carries
//! a partitioned `(span, class)` list — never one class for a mixed
//! aggregate. A `DurableEligible` sub-span's occurrences enter the
//! durable award lane (never rolled back, never re-awarded); an
//! `EncounterProvisional` sub-span's occurrences are governed by the
//! encounter machinery and roll back with their transaction — **never
//! silently promoted to permanent progress after a terminated
//! encounter**.
//!
//! **The uniform per-occurrence unit — an explicit, tested
//! invariant:** each covered occurrence contributes exactly `+1`
//! milestone and `+1` signal into one lane (`+1` coverage-insert per
//! crossing). A span-bearing envelope's delta must be exactly
//! `unit × occurrence-count` with the movement in a single lane —
//! proportional scaling is valid **only** through this unit, never
//! arbitrary counter arithmetic (that is what makes partial replay
//! exact). A span-less envelope is a net-zero lane movement (an
//! activation commit `{ledger: −S, credits: +S}`), applied
//! whole-or-not by identity.
//!
//! **Exact partial replay:** [`ConsequenceEnvelope::missing_against`]
//! computes the still-missing occurrences per class against an
//! anchor's **durable** coverage via [`Coverage::missing_of`] — exact
//! against hole-bearing anchors (an anchor covering {4, 6} against
//! the span `(3, 7]` yields exactly occurrences {5, 7}) — and scales
//! the per-class deltas by the missing counts. Whole-or-not by
//! construction: the caller applies the returned
//! [`PartialApplication`] or none of it; an envelope is never
//! partially skipped.
//!
//! **Suppression never rests on counters:** the expected-prior values
//! are integrity and causality checks — recorded and carried, never a
//! standalone identity proof (identity decides, counters validate);
//! journal intents never suppress replay; only the confirmed chain
//! and the anchor's proven coverage suppress (the pipeline and
//! recovery modules own that rule — this module supplies the exact
//! arithmetic). The **verified source baseline** (counter values +
//! the award-state checkpoint observed at preparation) anchors the
//! cumulative reconciliation: its durable lane is the `(anchor,
//! journal)` reconciliation base; the full checkpoint is the
//! encounter-rollback material.
//!
//! **The `(lo, hi]` convention throughout:** exclusive lower,
//! inclusive upper — identical in serialization, recovery, and tests.

use engine_core::{Decoder, Encoder, SaveError};

use crate::consequence::{AwardCheckpoint, ConsequenceId, Coverage, OccurrenceSpan};
use crate::hash::Hash;

/// The envelope domain (versioned; the schema byte below is the wire
/// version).
pub const ENVELOPE_DOMAIN: &str = "sektor7/engine-persist/envelope/1";
pub const ENVELOPE_SCHEMA: u8 = 1;

/// Documented maximum for one envelope's classified sub-spans
/// (bounded decode: each encoded sub-span is 17 bytes, so the count
/// is also bounded by the remaining input).
pub const MAX_ENVELOPE_SUB_SPANS: u64 = 1 << 16;
/// Documented maximum for one envelope's causal dependencies (each
/// encoded dependency is 16 bytes).
pub const MAX_ENVELOPE_DEPENDENCIES: u64 = 1 << 16;

/// The ratified durability classification of a covered occurrence
/// sub-span (the v1.8 rollback table) — the classification the engine
/// records and enforces while gameplay policy decides which class
/// applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityClass {
    /// The contribution lives in the durable lane — outside the
    /// encounter transaction. **Never rolled back** by an encounter
    /// failure: a rollback-then-reattempt cycle cannot duplicate it.
    DurableEligible,
    /// The contribution lives in the encounter transaction and
    /// **rolls back with it** — the restored eligibility permits the
    /// legitimate reattempt award. Never promoted to permanent
    /// progress after a terminated encounter.
    EncounterProvisional,
}

/// Which authoritative signal lane one occurrence's signal enters
/// (the uniform unit's lane dispositions: a crossing feeds the
/// pending lane at execution, the capture boundary moves it to the
/// captured lane, an activation commit moves captured signals to
/// credits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalLane {
    Pending,
    Captured,
    Credits,
}

/// The authoritative consequential counters one envelope may touch —
/// **enumerated exactly**. `steps` and routine-progression counters
/// are not members and can never appear in envelope state or deltas
/// (actual-execution state, never fabricated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CounterValues {
    /// `milestones[npc]` — awarded milestone count.
    pub milestones: u64,
    /// `pending_signals[npc]` — crossing signals awaiting capture.
    pub pending_signals: u64,
    /// `captured[npc]` — the capture-ledger lane.
    pub captured: u64,
    /// `credits[npc]` — activated, committed rewards.
    pub credits: u64,
}

impl CounterValues {
    /// The consequential conservation invariant
    /// (`milestones == credits + pending_signals + captured`), with
    /// checked arithmetic — an overflowing right-hand side is not
    /// conserved.
    pub fn conservation_holds(&self) -> bool {
        self.credits
            .checked_add(self.pending_signals)
            .and_then(|sum| sum.checked_add(self.captured))
            .is_some_and(|right| right == self.milestones)
    }

    fn encode_into(&self, enc: &mut Encoder) {
        enc.write_u64(self.milestones);
        enc.write_u64(self.pending_signals);
        enc.write_u64(self.captured);
        enc.write_u64(self.credits);
    }

    fn decode_from(dec: &mut Decoder<'_>) -> Result<Self, SaveError> {
        Ok(CounterValues {
            milestones: dec.read_u64()?,
            pending_signals: dec.read_u64()?,
            captured: dec.read_u64()?,
            credits: dec.read_u64()?,
        })
    }
}

/// The complete typed counter-delta — every authoritative counter the
/// real event touched, applied together (atomically). Signed lane
/// counters express movements between lanes (a capture moves signals
/// out of pending; an activation moves them out of the ledger);
/// milestones only ever count awards (monotone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CounterDelta {
    /// Milestone awards: `+1` per covered occurrence (the uniform
    /// unit; awards only add).
    pub milestones: u64,
    /// Net pending-lane movement (signed).
    pub pending_signals: i64,
    /// Net captured-lane movement (signed).
    pub captured: i64,
    /// Net credits movement (signed).
    pub credits: i64,
}

impl CounterDelta {
    /// The uniform per-occurrence unit delta for `count` occurrences
    /// whose signals enter `lane`: exactly `+count` milestones and
    /// `+count` signals into that one lane — the only shape whose
    /// occurrence-scaled partial application is exact.
    pub fn uniform(lane: SignalLane, count: u64) -> Result<Self, SaveError> {
        let signals = i64::try_from(count)
            .map_err(|_| SaveError::Integrity("envelope/occurrence-count-overflow"))?;
        let (pending_signals, captured, credits) = match lane {
            SignalLane::Pending => (signals, 0, 0),
            SignalLane::Captured => (0, signals, 0),
            SignalLane::Credits => (0, 0, signals),
        };
        Ok(CounterDelta {
            milestones: count,
            pending_signals,
            captured,
            credits,
        })
    }

    /// The net signal-lane movement (checked — arithmetic overflow is
    /// an integrity failure, never wrapped).
    fn net_signals(&self) -> Result<i64, SaveError> {
        self.pending_signals
            .checked_add(self.captured)
            .and_then(|sum| sum.checked_add(self.credits))
            .ok_or(SaveError::Integrity("envelope/delta-arithmetic-overflow"))
    }

    /// Checked, **pure** application — the whole delta or none of it
    /// (a partially-applied delta is unrepresentable: every field is
    /// computed before any is committed). Overflow fails closed.
    pub fn apply_to(&self, values: &CounterValues) -> Result<CounterValues, SaveError> {
        Ok(CounterValues {
            milestones: values
                .milestones
                .checked_add(self.milestones)
                .ok_or(SaveError::Integrity("envelope/counter-overflow"))?,
            pending_signals: apply_signed(values.pending_signals, self.pending_signals)?,
            captured: apply_signed(values.captured, self.captured)?,
            credits: apply_signed(values.credits, self.credits)?,
        })
    }

    pub fn is_zero(&self) -> bool {
        self.milestones == 0 && self.pending_signals == 0 && self.captured == 0 && self.credits == 0
    }
}

/// One class-homogeneous occurrence sub-span: the occurrences of
/// `(lo, hi]` covered under one durability classification. A mixed
/// aggregate carries several of these — the partition requirement —
/// and partial replay applies exactly the corresponding sub-span's
/// deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifiedSpan {
    pub span: OccurrenceSpan,
    pub class: DurabilityClass,
}

impl ClassifiedSpan {
    pub fn new(span: OccurrenceSpan, class: DurabilityClass) -> Self {
        ClassifiedSpan { span, class }
    }

    /// The occurrence count (`hi − lo`; the span is non-empty by
    /// construction).
    pub fn occurrences(&self) -> u64 {
        self.span.hi - self.span.lo
    }
}

/// The source-position binding (§5.4.1's temporal-discriminator
/// material): where the source stood when the envelope was prepared.
/// Region and episode live in the [`ConsequenceId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    /// The campaign head revision observed at preparation — the
    /// recovery-anchor binding (`None` before the founding append).
    pub head: Option<Hash>,
    /// The campaign tick at preparation.
    pub tick: u64,
}

/// The verified source baseline observed at preparation (read-only
/// source state): the counters and the award-state checkpoint the
/// envelope's delta causally builds on. The checkpoint's **durable**
/// lane is the `(anchor, journal)` reconciliation base; the full
/// checkpoint (both lanes) is the encounter-rollback material
/// (§5.4.1's award-state metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBaseline {
    pub counters: CounterValues,
    pub award: AwardCheckpoint,
}

/// The complete consequential-state envelope — the T1 journal's
/// record for one consequence identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsequenceEnvelope {
    /// The stable identity — never reused; a retry preserves it; a
    /// genuinely new operation receives a fresh one (the
    /// idempotency-key material).
    pub id: ConsequenceId,
    /// Class-homogeneous occurrence sub-spans — the partition of this
    /// aggregate's covered occurrences by durability class (sorted,
    /// disjoint; holes between spans are legitimate rolled-back
    /// occurrences this capture does not cover).
    pub sub_spans: Vec<ClassifiedSpan>,
    /// The complete typed counter-delta (the uniform per-occurrence
    /// unit for span-bearing envelopes; a net-zero lane movement for
    /// span-less ones).
    pub delta: CounterDelta,
    /// Expected-prior counter values — integrity and causality
    /// checks, never a standalone identity proof.
    pub expected_prior: CounterValues,
    /// The verified source baseline observed at preparation.
    pub baseline: SourceBaseline,
    /// The source-position binding.
    pub position: SourcePosition,
    /// Causal dependencies — consequence identities whose envelopes
    /// must apply before this one (strictly ascending canonical
    /// order, unique).
    pub dependencies: Vec<ConsequenceId>,
}

impl ConsequenceEnvelope {
    /// An empty envelope (span-less, zero delta, empty baseline) —
    /// compose with the `with_*` builders or build via
    /// [`Self::capture`].
    pub fn new(id: ConsequenceId) -> Self {
        ConsequenceEnvelope {
            id,
            sub_spans: Vec::new(),
            delta: CounterDelta::default(),
            expected_prior: CounterValues::default(),
            baseline: SourceBaseline {
                counters: CounterValues::default(),
                award: AwardCheckpoint {
                    durable: Coverage::new(),
                    provisional: Coverage::new(),
                },
            },
            position: SourcePosition {
                head: None,
                tick: 0,
            },
            dependencies: Vec::new(),
        }
    }

    /// The canonical capture envelope: `sub_spans` under the uniform
    /// per-occurrence unit into `lane` — the delta is **derived**
    /// (`+1 milestone, +1 signal per occurrence`), never hand-set, so
    /// a capture envelope cannot express a non-uniform delta.
    pub fn capture(
        id: ConsequenceId,
        sub_spans: Vec<ClassifiedSpan>,
        lane: SignalLane,
        expected_prior: CounterValues,
        baseline: SourceBaseline,
        position: SourcePosition,
    ) -> Result<Self, SaveError> {
        let mut count: u64 = 0;
        for classified in &sub_spans {
            count = count
                .checked_add(classified.occurrences())
                .ok_or(SaveError::Integrity("envelope/occurrence-overflow"))?;
        }
        let delta = CounterDelta::uniform(lane, count)?;
        let envelope = ConsequenceEnvelope {
            id,
            sub_spans,
            delta,
            expected_prior,
            baseline,
            position,
            dependencies: Vec::new(),
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn with_sub_span(mut self, span: OccurrenceSpan, class: DurabilityClass) -> Self {
        self.sub_spans.push(ClassifiedSpan { span, class });
        self
    }

    /// Sets an explicit delta — for span-less lane movements (an
    /// activation commit `{captured: −S, credits: +S}`). Validated at
    /// the next codec or replay boundary, never silently repaired.
    pub fn with_delta(mut self, delta: CounterDelta) -> Self {
        self.delta = delta;
        self
    }

    pub fn with_expected_prior(mut self, prior: CounterValues) -> Self {
        self.expected_prior = prior;
        self
    }

    pub fn with_baseline(mut self, baseline: SourceBaseline) -> Self {
        self.baseline = baseline;
        self
    }

    pub fn with_position(mut self, position: SourcePosition) -> Self {
        self.position = position;
        self
    }

    /// Attaches causal dependencies (normalized to ascending
    /// canonical order with duplicates removed — the decode-side
    /// strict check makes noncanonical input unrepresentable on the
    /// wire).
    pub fn with_dependencies(mut self, mut dependencies: Vec<ConsequenceId>) -> Self {
        dependencies.sort_unstable();
        dependencies.dedup();
        self.dependencies = dependencies;
        self
    }

    /// The total occurrences covered across all sub-spans (checked —
    /// overflow is an integrity failure).
    pub fn occurrences(&self) -> Result<u64, SaveError> {
        let mut total: u64 = 0;
        for classified in &self.sub_spans {
            total = total
                .checked_add(classified.occurrences())
                .ok_or(SaveError::Integrity("envelope/occurrence-overflow"))?;
        }
        Ok(total)
    }

    /// The single signal lane a span-bearing envelope's occurrences
    /// feed (inferred from the validated delta pattern); `None` for a
    /// span-less envelope, where no lane applies.
    pub fn signal_lane(&self) -> Result<Option<SignalLane>, SaveError> {
        let milestones = i64::try_from(self.delta.milestones)
            .map_err(|_| SaveError::Integrity("envelope/delta-arithmetic-overflow"))?;
        if milestones == 0 {
            return Ok(None);
        }
        let lanes = [
            (SignalLane::Pending, self.delta.pending_signals),
            (SignalLane::Captured, self.delta.captured),
            (SignalLane::Credits, self.delta.credits),
        ];
        for (lane, movement) in lanes {
            if movement == milestones {
                return Ok(Some(lane));
            }
        }
        Err(SaveError::Integrity("envelope/uniform-unit-violation"))
    }

    /// Structural and unit-invariant validation (fail-closed; run by
    /// `canonical_bytes`, `decode`, and `missing_against`).
    pub fn validate(&self) -> Result<(), SaveError> {
        // Sub-span canonical form: individually non-empty, strictly
        // ascending, disjoint (a shared occurrence is a per-occurrence
        // conflict — two classifications claiming one occurrence, or
        // one occurrence twice — and fails closed), and maximally
        // composed per class (same-class touching spans would have
        // composed). Gaps are legitimate: a hole is a rolled-back
        // occurrence this capture does not cover, preserved exactly —
        // never treated as covered.
        let mut prev: Option<ClassifiedSpan> = None;
        for classified in &self.sub_spans {
            if classified.span.lo >= classified.span.hi {
                return Err(SaveError::Integrity("envelope/sub-span-inverted"));
            }
            if let Some(prior) = prev {
                if classified.span.lo <= prior.span.lo {
                    return Err(SaveError::Integrity("envelope/sub-span-unsorted"));
                }
                if classified.span.lo < prior.span.hi {
                    return Err(SaveError::Integrity("envelope/sub-span-overlap"));
                }
                if classified.span.lo == prior.span.hi && prior.class == classified.class {
                    return Err(SaveError::Integrity("envelope/sub-span-noncanonical"));
                }
            }
            prev = Some(*classified);
        }
        // The uniform per-occurrence unit: a span-bearing delta is
        // exactly `unit × occurrence-count` (+1 milestone, +1 signal
        // into one lane per occurrence), and the net lane movement
        // always equals the milestone count — the delta form of the
        // conservation equation. A span-less delta is a net-zero lane
        // movement.
        let count = self.occurrences()?;
        let milestones = self.delta.milestones;
        if milestones != count {
            return Err(SaveError::Integrity("envelope/uniform-unit-violation"));
        }
        let net = self.delta.net_signals()?;
        let net_u64 = u64::try_from(net)
            .map_err(|_| SaveError::Integrity("envelope/uniform-unit-violation"))?;
        if net_u64 != milestones {
            return Err(SaveError::Integrity("envelope/uniform-unit-violation"));
        }
        if count > 0 {
            // Unit purity: exactly one lane carries `+milestones` and
            // the other two are zero — only then is partial replay's
            // occurrence scaling exact (a mixed-lane or
            // extra-movement delta cannot say which occurrences went
            // where).
            let unit = i64::try_from(milestones)
                .map_err(|_| SaveError::Integrity("envelope/delta-arithmetic-overflow"))?;
            let lanes = [
                self.delta.pending_signals,
                self.delta.captured,
                self.delta.credits,
            ];
            if lanes.iter().filter(|&&lane| lane == unit).count() != 1
                || lanes.iter().filter(|&&lane| lane == 0).count() != 2
            {
                return Err(SaveError::Integrity("envelope/uniform-unit-violation"));
            }
        }
        // Causal dependencies: strictly ascending, unique.
        for pair in self.dependencies.windows(2) {
            if pair[1] <= pair[0] {
                return Err(SaveError::Order {
                    section: "envelope",
                });
            }
        }
        // A coherent baseline never places one occurrence in both
        // award lanes.
        if self
            .baseline
            .award
            .durable
            .overlaps(&self.baseline.award.provisional)
        {
            return Err(SaveError::Integrity("envelope/baseline-cross-lane-overlap"));
        }
        Ok(())
    }

    /// Canonical bytes (versioned domain; bounded counts; only valid,
    /// unit-coherent envelopes serialize).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SaveError> {
        self.validate()?;
        const SECTION: &str = "envelope";
        let mut enc = Encoder::new();
        enc.write_str(ENVELOPE_DOMAIN);
        enc.write_u8(ENVELOPE_SCHEMA);
        enc.write_u32(self.id.region);
        enc.write_u64(self.id.episode);
        enc.write_u32(self.id.npc);
        if self.sub_spans.len() as u64 > MAX_ENVELOPE_SUB_SPANS {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "sub-span-count",
                value: self.sub_spans.len() as u64,
                max: MAX_ENVELOPE_SUB_SPANS,
            });
        }
        enc.write_count(self.sub_spans.len());
        for classified in &self.sub_spans {
            enc.write_u64(classified.span.lo);
            enc.write_u64(classified.span.hi);
            enc.write_u8(durability_discriminant(classified.class));
        }
        write_delta(&mut enc, &self.delta);
        self.expected_prior.encode_into(&mut enc);
        self.baseline.counters.encode_into(&mut enc);
        self.baseline.award.durable.encode_into(&mut enc)?;
        self.baseline.award.provisional.encode_into(&mut enc)?;
        match self.position.head {
            Some(head) => {
                enc.write_u8(1);
                enc.write_bytes(head.as_bytes());
            }
            None => enc.write_u8(0),
        }
        enc.write_u64(self.position.tick);
        if self.dependencies.len() as u64 > MAX_ENVELOPE_DEPENDENCIES {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "dependency-count",
                value: self.dependencies.len() as u64,
                max: MAX_ENVELOPE_DEPENDENCIES,
            });
        }
        enc.write_count(self.dependencies.len());
        for dep in &self.dependencies {
            enc.write_u32(dep.region);
            enc.write_u64(dep.episode);
            enc.write_u32(dep.npc);
        }
        Ok(enc.into_bytes())
    }

    /// Strict decode (fail-closed: domain, schema, enum
    /// discriminants, bounded counts — also bounded by the remaining
    /// input — per-span validity, then full structural and
    /// unit-invariant validation; no trailing bytes).
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "envelope";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(ENVELOPE_DOMAIN)?;
        let schema = dec.read_u8()?;
        if schema != ENVELOPE_SCHEMA {
            return Err(SaveError::Version {
                observed: u32::from(schema),
                supported: u32::from(ENVELOPE_SCHEMA),
            });
        }
        let id = ConsequenceId {
            region: dec.read_u32()?,
            episode: dec.read_u64()?,
            npc: dec.read_u32()?,
        };
        let count = dec.read_count(MAX_ENVELOPE_SUB_SPANS, SECTION, "sub-span-count")?;
        // Each encoded sub-span is exactly 17 bytes (16 span + 1
        // class).
        let remaining = dec.remaining() as u64;
        if count > remaining / 17 {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "sub-span-count",
                value: count,
                max: remaining / 17,
            });
        }
        let mut sub_spans: Vec<ClassifiedSpan> = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let lo = dec.read_u64()?;
            let hi = dec.read_u64()?;
            if lo >= hi {
                return Err(SaveError::Integrity("envelope/sub-span-inverted"));
            }
            let class = read_durability(&mut dec)?;
            sub_spans.push(ClassifiedSpan {
                span: OccurrenceSpan { lo, hi },
                class,
            });
        }
        let delta = read_delta(&mut dec)?;
        let expected_prior = CounterValues::decode_from(&mut dec)?;
        let counters = CounterValues::decode_from(&mut dec)?;
        let durable = Coverage::decode_from(&mut dec)?;
        let provisional = Coverage::decode_from(&mut dec)?;
        let head = match dec.read_u8()? {
            0 => None,
            1 => Some(read_hash(&mut dec)?),
            discriminant => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(discriminant),
                });
            }
        };
        let tick = dec.read_u64()?;
        let dep_count = dec.read_count(MAX_ENVELOPE_DEPENDENCIES, SECTION, "dependency-count")?;
        // Each encoded dependency is exactly 16 bytes.
        let remaining = dec.remaining() as u64;
        if dep_count > remaining / 16 {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "dependency-count",
                value: dep_count,
                max: remaining / 16,
            });
        }
        let mut dependencies: Vec<ConsequenceId> = Vec::with_capacity(dep_count as usize);
        for _ in 0..dep_count {
            dependencies.push(ConsequenceId {
                region: dec.read_u32()?,
                episode: dec.read_u64()?,
                npc: dec.read_u32()?,
            });
        }
        dec.finish()?;
        let envelope = ConsequenceEnvelope {
            id,
            sub_spans,
            delta,
            expected_prior,
            baseline: SourceBaseline {
                counters,
                award: AwardCheckpoint {
                    durable,
                    provisional,
                },
            },
            position: SourcePosition { head, tick },
            dependencies,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// Exact partial replay against an anchor's **durable** coverage
    /// (§5.4.1's suppression tiers: the loaded anchor's proven
    /// coverage suppresses; journal intents never do). Returns the
    /// still-missing occurrences — per class, exact against
    /// hole-bearing anchors via [`Coverage::missing_of`] — with the
    /// uniform-unit-scaled deltas for each class. Whole-or-not by
    /// construction: the caller applies the returned application or
    /// none of it.
    pub fn missing_against(
        &self,
        anchor_durable: &Coverage,
    ) -> Result<PartialApplication, SaveError> {
        self.validate()?;
        let mut missing: Vec<ClassifiedSpan> = Vec::new();
        let mut durable_count: u64 = 0;
        let mut provisional_count: u64 = 0;
        for classified in &self.sub_spans {
            for piece in anchor_durable.missing_of(&classified.span) {
                let occurrences = piece.hi - piece.lo;
                let destination = match classified.class {
                    DurabilityClass::DurableEligible => &mut durable_count,
                    DurabilityClass::EncounterProvisional => &mut provisional_count,
                };
                *destination = destination
                    .checked_add(occurrences)
                    .ok_or(SaveError::Integrity("envelope/occurrence-overflow"))?;
                missing.push(ClassifiedSpan {
                    span: piece,
                    class: classified.class,
                });
            }
        }
        // A span-less envelope has no lane (and no missing
        // occurrences); the default is inert.
        let lane = self.signal_lane()?.unwrap_or(SignalLane::Captured);
        Ok(PartialApplication {
            id: self.id,
            missing,
            durable_delta: CounterDelta::uniform(lane, durable_count)?,
            provisional_delta: CounterDelta::uniform(lane, provisional_count)?,
        })
    }
}

/// The exact partial application of one envelope against an anchor's
/// durable coverage: the still-missing classified occurrences and the
/// uniform-unit-scaled counter deltas, **separated by class** so the
/// caller routes each lane per its contract — durable occurrences
/// enter the authoritative counters and the durable award lane;
/// provisional occurrences are governed by the encounter machinery
/// and never silently promoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialApplication {
    pub id: ConsequenceId,
    /// The still-missing class-homogeneous sub-spans (sorted,
    /// disjoint).
    pub missing: Vec<ClassifiedSpan>,
    /// The uniform-unit delta over the missing `DurableEligible`
    /// occurrences.
    pub durable_delta: CounterDelta,
    /// The uniform-unit delta over the missing
    /// `EncounterProvisional` occurrences.
    pub provisional_delta: CounterDelta,
}

impl PartialApplication {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.durable_delta.is_zero() && self.provisional_delta.is_zero()
    }

    /// The total missing occurrences (checked).
    pub fn missing_count(&self) -> Result<u64, SaveError> {
        let mut total: u64 = 0;
        for classified in &self.missing {
            total = total
                .checked_add(classified.occurrences())
                .ok_or(SaveError::Integrity("envelope/occurrence-overflow"))?;
        }
        Ok(total)
    }

    /// The missing spans of one class.
    pub fn spans_of(&self, class: DurabilityClass) -> Vec<OccurrenceSpan> {
        self.missing
            .iter()
            .filter(|classified| classified.class == class)
            .map(|classified| classified.span)
            .collect()
    }
}

fn durability_discriminant(class: DurabilityClass) -> u8 {
    match class {
        DurabilityClass::DurableEligible => 0,
        DurabilityClass::EncounterProvisional => 1,
    }
}

fn read_durability(dec: &mut Decoder<'_>) -> Result<DurabilityClass, SaveError> {
    match dec.read_u8()? {
        0 => Ok(DurabilityClass::DurableEligible),
        1 => Ok(DurabilityClass::EncounterProvisional),
        discriminant => Err(SaveError::Enum {
            section: "envelope",
            discriminant: u64::from(discriminant),
        }),
    }
}

/// Signed lane movements ride the wire as two's-complement bit
/// patterns in the fixed LE `u64` width — value-preserving round
/// trips, platform-independent.
fn write_delta(enc: &mut Encoder, delta: &CounterDelta) {
    enc.write_u64(delta.milestones);
    enc.write_u64(delta.pending_signals as u64);
    enc.write_u64(delta.captured as u64);
    enc.write_u64(delta.credits as u64);
}

fn read_delta(dec: &mut Decoder<'_>) -> Result<CounterDelta, SaveError> {
    Ok(CounterDelta {
        milestones: dec.read_u64()?,
        pending_signals: dec.read_u64()? as i64,
        captured: dec.read_u64()? as i64,
        credits: dec.read_u64()? as i64,
    })
}

fn read_hash(dec: &mut Decoder<'_>) -> Result<Hash, SaveError> {
    let bytes = dec.read_bytes(Hash::WIDTH, "envelope")?;
    Hash::from_bytes(bytes).ok_or(SaveError::Integrity("envelope/hash-width"))
}

fn apply_signed(value: u64, delta: i64) -> Result<u64, SaveError> {
    value
        .checked_add_signed(delta)
        .ok_or(SaveError::Integrity("envelope/counter-overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_id() -> ConsequenceId {
        ConsequenceId::new(3, 7, 12)
    }

    fn classified(lo: u64, hi: u64, class: DurabilityClass) -> ClassifiedSpan {
        ClassifiedSpan {
            span: OccurrenceSpan::new(lo, hi).unwrap(),
            class,
        }
    }

    fn coverage_of(spans: &[(u64, u64)]) -> Coverage {
        let mut coverage = Coverage::new();
        for &(lo, hi) in spans {
            coverage.insert_span(OccurrenceSpan::new(lo, hi).unwrap());
        }
        coverage
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

    fn bare_position() -> SourcePosition {
        SourcePosition {
            head: None,
            tick: 0,
        }
    }

    fn sample_envelope() -> ConsequenceEnvelope {
        // Occurrences 4-5 durable, 7-8 provisional (holes at 3 and 6 —
        // rolled-back occurrences this capture does not cover); the
        // pre-event state holds occurrences 1-2 captured.
        ConsequenceEnvelope::capture(
            sample_id(),
            vec![
                classified(3, 5, DurabilityClass::DurableEligible),
                classified(6, 8, DurabilityClass::EncounterProvisional),
            ],
            SignalLane::Captured,
            CounterValues {
                milestones: 2,
                captured: 2,
                ..CounterValues::default()
            },
            SourceBaseline {
                counters: CounterValues {
                    milestones: 2,
                    captured: 2,
                    ..CounterValues::default()
                },
                award: AwardCheckpoint {
                    durable: coverage_of(&[(0, 2)]),
                    provisional: Coverage::new(),
                },
            },
            SourcePosition {
                head: Some(Hash::from_bytes(&[7u8; 32]).unwrap()),
                tick: 110,
            },
        )
        .unwrap()
        .with_dependencies(vec![
            ConsequenceId::new(3, 6, 12),
            ConsequenceId::new(3, 5, 12),
        ])
    }

    // ------------------------------------------------------------ codec

    #[test]
    fn capture_envelope_round_trips() {
        let envelope = sample_envelope();
        let bytes = envelope.canonical_bytes().unwrap();
        assert_eq!(bytes, envelope.canonical_bytes().unwrap());
        let decoded = ConsequenceEnvelope::decode(&bytes).unwrap();
        assert_eq!(decoded, envelope);
        assert_eq!(decoded.canonical_bytes().unwrap(), bytes);
    }

    #[test]
    fn minimal_envelope_round_trips() {
        let envelope = ConsequenceEnvelope::new(sample_id());
        assert!(envelope.validate().is_ok());
        let bytes = envelope.canonical_bytes().unwrap();
        assert_eq!(ConsequenceEnvelope::decode(&bytes).unwrap(), envelope);
    }

    #[test]
    fn domain_schema_and_trailing_are_rejected() {
        let bytes = sample_envelope().canonical_bytes().unwrap();
        // Wrong domain.
        let mut wrong_domain = Vec::new();
        let mut enc = Encoder::new();
        enc.write_str("sektor7/other/1");
        wrong_domain.extend_from_slice(&enc.into_bytes());
        wrong_domain.extend_from_slice(&bytes[8 + ENVELOPE_DOMAIN.len()..]);
        assert!(matches!(
            ConsequenceEnvelope::decode(&wrong_domain),
            Err(SaveError::Domain { .. })
        ));
        // Wrong schema.
        let mut wrong_schema = bytes.clone();
        let schema_at = 8 + ENVELOPE_DOMAIN.len();
        wrong_schema[schema_at] = 9;
        assert!(matches!(
            ConsequenceEnvelope::decode(&wrong_schema),
            Err(SaveError::Version { .. })
        ));
        // Trailing bytes.
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            ConsequenceEnvelope::decode(&trailing),
            Err(SaveError::Trailing { .. })
        ));
    }

    #[test]
    fn truncation_rejected_at_every_boundary() {
        let bytes = sample_envelope().canonical_bytes().unwrap();
        for cut in 0..bytes.len() {
            assert!(
                ConsequenceEnvelope::decode(&bytes[..cut]).is_err(),
                "a {cut}-byte prefix must not decode"
            );
        }
        assert!(ConsequenceEnvelope::decode(&bytes).is_ok());
    }

    #[test]
    fn oversized_sub_span_count_rejected_before_allocation() {
        let mut enc = Encoder::new();
        enc.write_str(ENVELOPE_DOMAIN);
        enc.write_u8(ENVELOPE_SCHEMA);
        enc.write_u32(3);
        enc.write_u64(7);
        enc.write_u32(12);
        enc.write_u64(1 << 20);
        let bytes = enc.into_bytes();
        assert!(matches!(
            ConsequenceEnvelope::decode(&bytes),
            Err(SaveError::Limit { .. })
        ));
    }

    #[test]
    fn sub_span_count_bounded_by_remaining_input() {
        let mut enc = Encoder::new();
        enc.write_str(ENVELOPE_DOMAIN);
        enc.write_u8(ENVELOPE_SCHEMA);
        enc.write_u32(3);
        enc.write_u64(7);
        enc.write_u32(12);
        enc.write_u64(2); // two sub-spans claim 34 bytes...
        enc.write_bytes(&[0u8; 20]); // ...but only 28 remain
        let bytes = enc.into_bytes();
        assert!(matches!(
            ConsequenceEnvelope::decode(&bytes),
            Err(SaveError::Limit { .. })
        ));
    }

    #[test]
    fn unknown_enum_discriminants_are_rejected() {
        let mut enc = Encoder::new();
        enc.write_str(ENVELOPE_DOMAIN);
        enc.write_u8(ENVELOPE_SCHEMA);
        enc.write_u32(3);
        enc.write_u64(7);
        enc.write_u32(12);
        enc.write_count(1);
        enc.write_u64(0);
        enc.write_u64(1);
        enc.write_u8(7); // no such durability class
        let bytes = enc.into_bytes();
        assert!(matches!(
            ConsequenceEnvelope::decode(&bytes),
            Err(SaveError::Enum { .. })
        ));
    }

    // ---------------------------------------------------- the unit rule

    #[test]
    fn uniform_unit_milestones_must_match_occurrences() {
        let envelope = sample_envelope();
        // 4 covered occurrences; a milestone count of 3 is a
        // non-uniform delta — partial replay could not scale it.
        let tampered = envelope.with_delta(CounterDelta {
            milestones: 3,
            captured: 4,
            ..CounterDelta::default()
        });
        assert!(tampered.validate().is_err());
        assert!(tampered.canonical_bytes().is_err());
    }

    #[test]
    fn uniform_unit_requires_one_pure_lane() {
        let base = ConsequenceEnvelope::capture(
            sample_id(),
            vec![classified(0, 2, DurabilityClass::DurableEligible)],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        )
        .unwrap();
        // {milestones: 2, pending: 1, captured: 1}: the net matches,
        // but two lanes share the movement — not the uniform unit.
        let split = base.clone().with_delta(CounterDelta {
            milestones: 2,
            pending_signals: 1,
            captured: 1,
            credits: 0,
        });
        assert!(split.validate().is_err());
        // {milestones: 2, captured: 2, pending: -1, credits: +1}: one
        // lane matches, but net-zero movements ride along — partial
        // scaling would leave them ambiguous.
        let mixed = base.with_delta(CounterDelta {
            milestones: 2,
            pending_signals: -1,
            captured: 2,
            credits: 1,
        });
        assert!(mixed.validate().is_err());
    }

    #[test]
    fn span_less_net_zero_movements_validate() {
        // An activation commit: the ledger lane releases five signals
        // to credits — no new occurrences, applied whole-or-not by
        // identity.
        let activation = ConsequenceEnvelope::new(sample_id())
            .with_delta(CounterDelta {
                milestones: 0,
                pending_signals: 0,
                captured: -5,
                credits: 5,
            })
            .with_position(SourcePosition {
                head: None,
                tick: 42,
            });
        assert!(activation.validate().is_ok());
        let bytes = activation.canonical_bytes().unwrap();
        assert_eq!(ConsequenceEnvelope::decode(&bytes).unwrap(), activation);
        assert_eq!(
            activation.missing_against(&Coverage::new()).unwrap(),
            PartialApplication {
                id: sample_id(),
                missing: Vec::new(),
                durable_delta: CounterDelta::default(),
                provisional_delta: CounterDelta::default(),
            }
        );
        // A net-nonzero span-less delta breaks conservation.
        let unbalanced = ConsequenceEnvelope::new(sample_id()).with_delta(CounterDelta {
            milestones: 0,
            pending_signals: 0,
            captured: -5,
            credits: 4,
        });
        assert!(unbalanced.validate().is_err());
        // A milestone without its signal unit is incoherent.
        let orphan_award = ConsequenceEnvelope::new(sample_id()).with_delta(CounterDelta {
            milestones: 1,
            pending_signals: 0,
            captured: 0,
            credits: 0,
        });
        assert!(orphan_award.validate().is_err());
    }

    #[test]
    fn signed_lane_movements_round_trip() {
        let activation = ConsequenceEnvelope::new(sample_id())
            .with_delta(CounterDelta {
                milestones: 0,
                pending_signals: 0,
                captured: -5,
                credits: 5,
            })
            .with_position(SourcePosition {
                head: Some(Hash::from_bytes(&[9u8; 32]).unwrap()),
                tick: 200,
            });
        let bytes = activation.canonical_bytes().unwrap();
        let decoded = ConsequenceEnvelope::decode(&bytes).unwrap();
        assert_eq!(decoded.delta.captured, -5);
        assert_eq!(decoded.delta.credits, 5);
        assert_eq!(decoded, activation);
    }

    #[test]
    fn signal_lane_is_inferred_from_the_unit_pattern() {
        let build = |lane| {
            ConsequenceEnvelope::capture(
                sample_id(),
                vec![classified(0, 3, DurabilityClass::DurableEligible)],
                lane,
                CounterValues::default(),
                empty_baseline(),
                bare_position(),
            )
            .unwrap()
        };
        assert_eq!(
            build(SignalLane::Captured).signal_lane().unwrap(),
            Some(SignalLane::Captured)
        );
        assert_eq!(
            build(SignalLane::Pending).signal_lane().unwrap(),
            Some(SignalLane::Pending)
        );
        assert_eq!(
            ConsequenceEnvelope::new(sample_id()).signal_lane().unwrap(),
            None
        );
    }

    // ------------------------------------------------- sub-span form

    #[test]
    fn sub_span_overlap_is_a_per_occurrence_conflict() {
        // Occurrences 4-5 claimed by two classes — divergent
        // classification for one occurrence fails closed, never
        // repaired.
        let cross_class = ConsequenceEnvelope::capture(
            sample_id(),
            vec![
                classified(3, 5, DurabilityClass::DurableEligible),
                classified(4, 7, DurabilityClass::EncounterProvisional),
            ],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        );
        assert!(cross_class.is_err());
        // Same class overlapping is equally invalid.
        let same_class = ConsequenceEnvelope::capture(
            sample_id(),
            vec![
                classified(3, 5, DurabilityClass::DurableEligible),
                classified(4, 7, DurabilityClass::DurableEligible),
            ],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        );
        assert!(same_class.is_err());
    }

    #[test]
    fn same_class_adjacent_sub_spans_are_noncanonical() {
        // Same-lane touching spans would have composed; carrying both
        // is a non-canonical form.
        let result = ConsequenceEnvelope::capture(
            sample_id(),
            vec![
                classified(0, 5, DurabilityClass::DurableEligible),
                classified(5, 9, DurabilityClass::DurableEligible),
            ],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn different_class_adjacent_sub_spans_partition_legitimately() {
        // The mixed aggregate: occurrences 1-5 durable, 6-9
        // provisional — the class-homogeneous partition requirement.
        let envelope = ConsequenceEnvelope::capture(
            sample_id(),
            vec![
                classified(0, 5, DurabilityClass::DurableEligible),
                classified(5, 9, DurabilityClass::EncounterProvisional),
            ],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        )
        .unwrap();
        let bytes = envelope.canonical_bytes().unwrap();
        assert_eq!(ConsequenceEnvelope::decode(&bytes).unwrap(), envelope);
    }

    #[test]
    fn sub_span_gaps_are_legitimate_holes() {
        // A hole between this capture's spans is a rolled-back
        // occurrence the capture does not cover — preserved exactly,
        // never treated as covered.
        let envelope = ConsequenceEnvelope::capture(
            sample_id(),
            vec![
                classified(0, 5, DurabilityClass::DurableEligible),
                classified(6, 9, DurabilityClass::DurableEligible),
            ],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        )
        .unwrap();
        // (0,5] covers 5 occurrences, (6,9] covers 3 — occurrence 6
        // is the hole.
        assert_eq!(envelope.occurrences().unwrap(), 8);
        let bytes = envelope.canonical_bytes().unwrap();
        assert_eq!(ConsequenceEnvelope::decode(&bytes).unwrap(), envelope);
    }

    #[test]
    fn unsorted_sub_spans_are_rejected() {
        let result = ConsequenceEnvelope::capture(
            sample_id(),
            vec![
                classified(6, 8, DurabilityClass::DurableEligible),
                classified(3, 5, DurabilityClass::DurableEligible),
            ],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        );
        assert!(result.is_err());
        // A hand-encoded unsorted pair never decodes.
        let mut enc = Encoder::new();
        enc.write_str(ENVELOPE_DOMAIN);
        enc.write_u8(ENVELOPE_SCHEMA);
        enc.write_u32(3);
        enc.write_u64(7);
        enc.write_u32(12);
        enc.write_count(2);
        enc.write_u64(6);
        enc.write_u64(8);
        enc.write_u8(0);
        enc.write_u64(3);
        enc.write_u64(5);
        enc.write_u8(0);
        write_delta(
            &mut enc,
            &CounterDelta::uniform(SignalLane::Captured, 4).unwrap(),
        );
        CounterValues::default().encode_into(&mut enc);
        CounterValues::default().encode_into(&mut enc);
        Coverage::new().encode_into(&mut enc).unwrap();
        Coverage::new().encode_into(&mut enc).unwrap();
        enc.write_u8(0);
        enc.write_u64(0);
        enc.write_count(0);
        let bytes = enc.into_bytes();
        assert!(matches!(
            ConsequenceEnvelope::decode(&bytes),
            Err(SaveError::Integrity(_))
        ));
    }

    // -------------------------------------------------- partial replay

    #[test]
    fn hole_bearing_anchor_missing_is_exact() {
        // The reviewer's counterexample at envelope level: an anchor
        // covering occurrences {4, 6} against the span (3, 7] yields
        // exactly the missing {5, 7} — a scalar high-watermark of 6
        // would falsely cover 5.
        let envelope = ConsequenceEnvelope::capture(
            sample_id(),
            vec![classified(3, 7, DurabilityClass::DurableEligible)],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        )
        .unwrap();
        let anchor = coverage_of(&[(3, 4), (5, 6)]);
        let application = envelope.missing_against(&anchor).unwrap();
        let spans: Vec<(u64, u64)> = application
            .spans_of(DurabilityClass::DurableEligible)
            .iter()
            .map(|span| (span.lo, span.hi))
            .collect();
        assert_eq!(spans, vec![(4, 5), (6, 7)]);
        assert_eq!(application.durable_delta.milestones, 2);
        assert_eq!(application.durable_delta.captured, 2);
        assert!(application.provisional_delta.is_zero());
    }

    #[test]
    fn mixed_class_partial_replay_splits_deltas_by_class() {
        // One aggregate ConsequenceId, two classes: occurrences 4-5
        // durable, 7-8 provisional. The anchor already covers 4 —
        // partial replay applies exactly occurrence 5 to the durable
        // path and occurrences 7-8 to the provisional path.
        let envelope = ConsequenceEnvelope::capture(
            sample_id(),
            vec![
                classified(3, 5, DurabilityClass::DurableEligible),
                classified(6, 8, DurabilityClass::EncounterProvisional),
            ],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            bare_position(),
        )
        .unwrap();
        assert_eq!(envelope.delta.milestones, 4);
        let anchor = coverage_of(&[(3, 4)]);
        let application = envelope.missing_against(&anchor).unwrap();
        let durable_spans = application.spans_of(DurabilityClass::DurableEligible);
        assert_eq!(
            durable_spans
                .iter()
                .map(|s| (s.lo, s.hi))
                .collect::<Vec<_>>(),
            vec![(4, 5)]
        );
        let provisional_spans = application.spans_of(DurabilityClass::EncounterProvisional);
        assert_eq!(
            provisional_spans
                .iter()
                .map(|s| (s.lo, s.hi))
                .collect::<Vec<_>>(),
            vec![(6, 8)]
        );
        assert_eq!(application.durable_delta.milestones, 1);
        assert_eq!(application.durable_delta.captured, 1);
        assert_eq!(application.provisional_delta.milestones, 2);
        assert_eq!(application.provisional_delta.captured, 2);
        assert_eq!(application.missing_count().unwrap(), 3);
    }

    #[test]
    fn exact_bound_coverage_at_lo_and_hi() {
        let build = || {
            ConsequenceEnvelope::capture(
                sample_id(),
                vec![classified(3, 7, DurabilityClass::DurableEligible)],
                SignalLane::Captured,
                CounterValues::default(),
                empty_baseline(),
                bare_position(),
            )
            .unwrap()
        };
        // Anchor at A == lo: the full span is missing.
        let at_lo = build().missing_against(&coverage_of(&[(0, 3)])).unwrap();
        assert_eq!(at_lo.missing_count().unwrap(), 4);
        assert_eq!(at_lo.durable_delta.milestones, 4);
        // Anchor at A == hi: nothing is missing — the (lo, hi]
        // convention verified at both bounds.
        let at_hi = build().missing_against(&coverage_of(&[(0, 7)])).unwrap();
        assert!(at_hi.is_empty());
    }

    #[test]
    fn older_anchor_walkthrough_restores_conservation() {
        // §5.4.1's walkthrough, counter level: the confirmed tick-100
        // anchor holds none of the crossing's state; the envelope
        // restores the complete delta atomically — milestones AND the
        // signal lane together — while `steps` (actual-execution
        // state, structurally absent from this schema) is never
        // fabricated.
        let envelope = ConsequenceEnvelope::capture(
            sample_id(),
            vec![classified(0, 1, DurabilityClass::DurableEligible)],
            SignalLane::Captured,
            CounterValues::default(),
            empty_baseline(),
            SourcePosition {
                head: None,
                tick: 110,
            },
        )
        .unwrap();
        let application = envelope.missing_against(&Coverage::new()).unwrap();
        let counters = application
            .durable_delta
            .apply_to(&CounterValues::default())
            .unwrap();
        assert_eq!(counters.milestones, 1);
        assert_eq!(counters.captured, 1);
        assert!(
            counters.conservation_holds(),
            "milestones == credits + pending_signals + captured"
        );
        // The award-state insert makes re-derivation idempotent — the
        // W4 shape: re-replay from the updated base finds nothing
        // missing, no duplication.
        let mut durable = Coverage::new();
        durable.insert_span(OccurrenceSpan::new(0, 1).unwrap());
        assert!(envelope.missing_against(&durable).unwrap().is_empty());
    }

    // ------------------------------------------------------ arithmetic

    #[test]
    fn application_is_checked_against_overflow() {
        let saturated = CounterValues {
            milestones: u64::MAX,
            ..CounterValues::default()
        };
        let delta = CounterDelta::uniform(SignalLane::Captured, 1).unwrap();
        assert!(delta.apply_to(&saturated).is_err());
        // Pure application: the saturated values are never partially
        // mutated.
        assert_eq!(saturated.milestones, u64::MAX);
        assert_eq!(saturated.captured, 0);
    }

    #[test]
    fn conservation_check_detects_mismatch_and_overflow() {
        assert!(CounterValues::default().conservation_holds());
        assert!(
            CounterValues {
                milestones: 3,
                captured: 2,
                credits: 1,
                ..CounterValues::default()
            }
            .conservation_holds()
        );
        // The §5.4.1 conservation break: only the signal half of a
        // crossing's delta applied — the right side grows without the
        // left.
        assert!(
            !CounterValues {
                milestones: 1,
                captured: 2,
                ..CounterValues::default()
            }
            .conservation_holds()
        );
        assert!(
            !CounterValues {
                milestones: 1,
                pending_signals: u64::MAX,
                captured: u64::MAX,
                credits: 0,
            }
            .conservation_holds()
        );
    }

    // ----------------------------------------------------- dependencies

    #[test]
    fn dependencies_are_canonically_ordered_and_unique() {
        let a = ConsequenceId::new(3, 5, 12);
        let b = ConsequenceId::new(3, 6, 12);
        let envelope = ConsequenceEnvelope::new(sample_id()).with_dependencies(vec![b, a, b]);
        assert_eq!(envelope.dependencies, vec![a, b]);
        // Descending input is an integrity failure, never repaired.
        let unsorted = ConsequenceEnvelope {
            dependencies: vec![b, a],
            ..ConsequenceEnvelope::new(sample_id())
        };
        assert!(matches!(unsorted.validate(), Err(SaveError::Order { .. })));
    }

    #[test]
    fn baseline_cross_lane_overlap_is_rejected() {
        // One occurrence in both award lanes is an incoherent
        // observed baseline — rejected, never repaired.
        let incoherent = SourceBaseline {
            counters: CounterValues::default(),
            award: AwardCheckpoint {
                durable: coverage_of(&[(0, 2)]),
                provisional: coverage_of(&[(1, 3)]),
            },
        };
        let envelope = ConsequenceEnvelope::new(sample_id()).with_baseline(incoherent);
        assert!(matches!(
            envelope.validate(),
            Err(SaveError::Integrity("envelope/baseline-cross-lane-overlap"))
        ));
    }
}
