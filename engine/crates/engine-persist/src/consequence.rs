//! The structured consequential identity, occurrence coverage, and
//! the class-aware award representation (§5.5, §5.4.1).
//!
//! **`ConsequenceId` (`consequence-id/1`)** — the versioned
//! structured identity replacing the Stage 4 packed `u64` key for all
//! durable pipeline state: schema `u8`, region `u32`, episode `u64`,
//! npc `u32`, fixed widths on the wire. No issued durable identity is
//! ever reused; a retry preserves its original identity; a genuinely
//! new operation receives a fresh one. The legacy in-memory packed
//! key and its fingerprint domain remain untouched (B4 owns any
//! migration).
//!
//! **`OccurrenceSpan`** — the ratified `(lo, hi]` convention
//! (exclusive lower, inclusive upper): an aggregate capture under one
//! `ConsequenceId` covers the occurrences `(lo, hi]`; adjacent spans
//! compose by union; a gap or inversion fails closed; partial anchor
//! coverage `A` leaves exactly `(max(lo, A), hi]` missing.
//!
//! **`AwardState`** — the class-aware awarded-occurrence
//! representation (the v1.8 rollback table). A single scalar
//! high-watermark is **insufficient** for mixed durability classes:
//! an encounter that retains a durable occurrence 4 while rolling
//! back a provisional occurrence 5 must leave 4 un-earnable and 5
//! legitimately earnable — a restored checkpoint scalar would
//! re-award 4 (duplicate) and a retained scalar would suppress 5
//! (loss). The smallest sufficient representation tracks the
//! **durable lane's** high-watermark separately from the
//! **encounter transaction's** provisional contribution: rollback
//! restores only the provisional value (durable contributions are
//! never rolled back); commit folds the provisional contribution
//! into the durable lane.

use std::fmt;

use engine_core::{Decoder, Encoder, SaveError};

/// The structured-identity domain (versioned; the version is the wire
/// schema byte below).
pub const CONSEQUENCE_ID_DOMAIN: &str = "sektor7/engine-persist/consequence-id/1";
/// The award-state checkpoint domain (versioned).
pub const AWARD_STATE_DOMAIN: &str = "sektor7/engine-persist/award-state/1";

pub const CONSEQUENCE_ID_SCHEMA: u8 = 1;
pub const AWARD_STATE_SCHEMA: u8 = 1;

/// Documented maxima (bounded decode, checked before allocation).
pub const MAX_REGION: u32 = u32::MAX;
pub const MAX_NPC: u32 = u32::MAX;

/// The structured consequential identity — region, episode, npc.
/// Ordered by (region, episode, npc) for canonical key-set ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConsequenceId {
    pub region: u32,
    pub episode: u64,
    pub npc: u32,
}

impl ConsequenceId {
    pub fn new(region: u32, episode: u64, npc: u32) -> Self {
        ConsequenceId {
            region,
            episode,
            npc,
        }
    }

    /// Canonical bytes — the stable wire form and the derived
    /// idempotency-key material (the identity never changes across
    /// retries).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(CONSEQUENCE_ID_DOMAIN);
        enc.write_u8(CONSEQUENCE_ID_SCHEMA);
        enc.write_u32(self.region);
        enc.write_u64(self.episode);
        enc.write_u32(self.npc);
        enc.into_bytes()
    }

    /// Strict decode (fail-closed: domain, schema, fixed widths, no
    /// trailing bytes).
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        let mut dec = Decoder::new(bytes);
        dec.read_domain(CONSEQUENCE_ID_DOMAIN)?;
        let schema = dec.read_u8()?;
        if schema != CONSEQUENCE_ID_SCHEMA {
            return Err(SaveError::Version {
                observed: u32::from(schema),
                supported: u32::from(CONSEQUENCE_ID_SCHEMA),
            });
        }
        let region = dec.read_u32()?;
        let episode = dec.read_u64()?;
        let npc = dec.read_u32()?;
        dec.finish()?;
        Ok(ConsequenceId {
            region,
            episode,
            npc,
        })
    }
}

impl fmt::Display for ConsequenceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "consequence r{} e{} n{}",
            self.region, self.episode, self.npc
        )
    }
}

/// A span error — gaps and inversions fail closed, never repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanError {
    /// `lo >= hi`: empty or inverted — incoherent for a durable
    /// envelope.
    Inverted,
    /// The spans do not touch or overlap: occurrences between them
    /// would be neither covered nor recoverable.
    Gap,
}

impl fmt::Display for SpanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpanError::Inverted => write!(f, "occurrence span is empty or inverted"),
            SpanError::Gap => write!(f, "occurrence spans leave a gap"),
        }
    }
}

impl std::error::Error for SpanError {}

/// The ratified occurrence convention: `(lo, hi]` — **exclusive
/// lower, inclusive upper**. Occurrence `k` is covered iff
/// `lo < k <= hi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccurrenceSpan {
    pub lo: u64,
    pub hi: u64,
}

impl OccurrenceSpan {
    /// A span is valid iff `lo < hi` (non-empty, non-inverted —
    /// empty or inverted ranges are incoherent and fail closed).
    pub fn new(lo: u64, hi: u64) -> Result<Self, SpanError> {
        if lo >= hi {
            return Err(SpanError::Inverted);
        }
        Ok(OccurrenceSpan { lo, hi })
    }

    /// Whether occurrence `k` is covered: `lo < k <= hi`.
    pub fn covers(&self, k: u64) -> bool {
        self.lo < k && k <= self.hi
    }

    /// The occurrences still missing against an anchor's proven
    /// coverage `A`: exactly `(max(lo, A), hi]`, or `None` when
    /// nothing is missing (`A >= hi`). The arithmetic is exact
    /// because the per-occurrence delta decomposition is uniform.
    pub fn missing_against(&self, covered: u64) -> Option<OccurrenceSpan> {
        if covered >= self.hi {
            return None;
        }
        OccurrenceSpan::new(self.lo.max(covered), self.hi).ok()
    }

    /// Whether the spans touch or overlap (`self` at-or-before
    /// `other`): composable without a gap.
    pub fn adjacent_or_overlaps(&self, other: &OccurrenceSpan) -> bool {
        other.lo <= self.hi
    }

    /// Union composition — valid for adjacent or overlapping spans
    /// (the uniform per-occurrence deltas make the union exact); a
    /// gap fails closed (the missing occurrences are neither covered
    /// by this span nor recoverable through it).
    pub fn compose(&self, other: &OccurrenceSpan) -> Result<OccurrenceSpan, SpanError> {
        if !self.adjacent_or_overlaps(other) {
            return Err(SpanError::Gap);
        }
        Ok(OccurrenceSpan {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        })
    }
}

impl fmt::Display for OccurrenceSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}]", self.lo, self.hi)
    }
}

/// The pre-encounter award-state snapshot — what a rollback restores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwardCheckpoint {
    pub durable: u64,
    pub provisional: u64,
}

/// The class-aware awarded-occurrence state for one NPC: the durable
/// lane's high-watermark and the encounter transaction's provisional
/// contribution, tracked separately.
///
/// **Why two values (the v1.8 rollback table):** a
/// `DurableEligible` award's contribution lives in the durable lane
/// — outside the encounter transaction — and is **never rolled
/// back** (a rollback-then-reattempt cycle cannot duplicate it); an
/// `EncounterProvisional` award's contribution lives in the
/// encounter transaction and **rolls back with it** (the checkpoint
/// restores the pre-encounter value, so the legitimate reattempt can
/// earn it again). A single scalar cannot express the mixed
/// interleaving — restoring it would duplicate the retained durable
/// occurrence, keeping it would suppress the rolled-back provisional
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwardState {
    /// The highest occurrence covered by durable-lane state
    /// (journal envelopes / snapshots / the confirmed chain) —
    /// monotonically advancing, never rolled back.
    pub durable: u64,
    /// The highest occurrence covered by the current encounter
    /// transaction's provisional contributions — restorable from the
    /// encounter checkpoint.
    pub provisional: u64,
}

impl AwardState {
    pub fn new() -> Self {
        AwardState {
            durable: 0,
            provisional: 0,
        }
    }

    /// The awarded-occurrence high-watermark the live eligibility
    /// check compares against: both lanes' contributions count while
    /// the encounter is live.
    pub fn awarded(&self) -> u64 {
        self.durable.max(self.provisional)
    }

    /// A durability-classified award at occurrence `k`: its
    /// contribution enters the durable lane (outside the encounter
    /// transaction) — retained across encounter failure, never
    /// re-earned.
    pub fn award_durable(&mut self, k: u64) {
        self.durable = self.durable.max(k);
    }

    /// An encounter-provisional award at occurrence `k`: its
    /// contribution lives in the encounter transaction — rolled back
    /// on failure, legitimately earnable again on the reattempt.
    pub fn award_provisional(&mut self, k: u64) {
        self.provisional = self.provisional.max(k);
    }

    /// The pre-encounter checkpoint a rollback restores to.
    pub fn checkpoint(&self) -> AwardCheckpoint {
        AwardCheckpoint {
            durable: self.durable,
            provisional: self.provisional,
        }
    }

    /// Encounter-failure rollback: restores the **provisional**
    /// contribution from the checkpoint; the **durable** lane is
    /// never rolled back (durable-classified awards from during the
    /// encounter are retained — their identities and eligibility
    /// protection stand).
    pub fn rollback(&mut self, checkpoint: AwardCheckpoint) {
        self.provisional = checkpoint.provisional;
    }

    /// Encounter commit: the transaction's provisional contributions
    /// are committed through their durability path and fold into the
    /// durable lane.
    pub fn commit_encounter(&mut self) {
        self.durable = self.durable.max(self.provisional);
        self.provisional = self.durable;
    }

    /// Canonical bytes (versioned, for journal transaction frames and
    /// checkpoints).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(AWARD_STATE_DOMAIN);
        enc.write_u8(AWARD_STATE_SCHEMA);
        enc.write_u64(self.durable);
        enc.write_u64(self.provisional);
        enc.into_bytes()
    }

    /// Strict decode (fail-closed).
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        let mut dec = Decoder::new(bytes);
        dec.read_domain(AWARD_STATE_DOMAIN)?;
        let schema = dec.read_u8()?;
        if schema != AWARD_STATE_SCHEMA {
            return Err(SaveError::Version {
                observed: u32::from(schema),
                supported: u32::from(AWARD_STATE_SCHEMA),
            });
        }
        let durable = dec.read_u64()?;
        let provisional = dec.read_u64()?;
        dec.finish()?;
        Ok(AwardState {
            durable,
            provisional,
        })
    }
}

impl Default for AwardState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------ identity

    #[test]
    fn consequence_id_round_trips() {
        let id = ConsequenceId::new(7, 42, 123);
        let bytes = id.canonical_bytes();
        assert_eq!(ConsequenceId::decode(&bytes).unwrap(), id);
    }

    #[test]
    fn consequence_id_is_deterministic_and_sensitive() {
        let a = ConsequenceId::new(1, 2, 3);
        assert_eq!(a.canonical_bytes(), a.canonical_bytes());
        assert_ne!(a.canonical_bytes(), ConsequenceId::new(2, 2, 3).canonical_bytes());
        assert_ne!(a.canonical_bytes(), ConsequenceId::new(1, 3, 3).canonical_bytes());
        assert_ne!(a.canonical_bytes(), ConsequenceId::new(1, 2, 4).canonical_bytes());
    }

    #[test]
    fn consequence_id_decode_rejects_malformed_input() {
        let bytes = ConsequenceId::new(1, 2, 3).canonical_bytes();
        // Wrong domain.
        let mut wrong = Vec::new();
        let mut enc = Encoder::new();
        enc.write_str("sektor7/other/1");
        wrong.extend_from_slice(&enc.into_bytes());
        wrong.extend_from_slice(&bytes[8 + CONSEQUENCE_ID_DOMAIN.len()..]);
        assert!(matches!(
            ConsequenceId::decode(&wrong),
            Err(SaveError::Domain { .. })
        ));
        // Wrong schema.
        let mut wrong_schema = bytes.clone();
        let schema_at = 8 + CONSEQUENCE_ID_DOMAIN.len();
        wrong_schema[schema_at] = 9;
        assert!(matches!(
            ConsequenceId::decode(&wrong_schema),
            Err(SaveError::Version { .. })
        ));
        // Truncation at every byte boundary.
        for cut in 0..bytes.len() {
            assert!(ConsequenceId::decode(&bytes[..cut]).is_err());
        }
        // Trailing bytes.
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            ConsequenceId::decode(&trailing),
            Err(SaveError::Trailing { .. })
        ));
    }

    #[test]
    fn consequence_id_orders_canonically() {
        let mut ids = vec![
            ConsequenceId::new(2, 1, 1),
            ConsequenceId::new(1, 2, 1),
            ConsequenceId::new(1, 1, 2),
            ConsequenceId::new(1, 1, 1),
        ];
        ids.sort();
        assert_eq!(
            ids,
            vec![
                ConsequenceId::new(1, 1, 1),
                ConsequenceId::new(1, 1, 2),
                ConsequenceId::new(1, 2, 1),
                ConsequenceId::new(2, 1, 1),
            ]
        );
    }

    // --------------------------------------------------------------- spans

    #[test]
    fn span_rejects_inversions_and_empty_ranges() {
        assert_eq!(OccurrenceSpan::new(5, 5), Err(SpanError::Inverted));
        assert_eq!(OccurrenceSpan::new(6, 5), Err(SpanError::Inverted));
        assert!(OccurrenceSpan::new(5, 6).is_ok());
    }

    #[test]
    fn span_boundaries_are_exclusive_lower_inclusive_upper() {
        let span = OccurrenceSpan::new(3, 7).unwrap();
        assert!(!span.covers(3), "lo is exclusive");
        assert!(span.covers(4));
        assert!(span.covers(7), "hi is inclusive");
        assert!(!span.covers(8));
    }

    #[test]
    fn missing_against_partial_anchor_coverage_is_the_suffix() {
        let span = OccurrenceSpan::new(3, 7).unwrap();
        // The anchor predates the span: everything is missing.
        assert_eq!(span.missing_against(0), Some(OccurrenceSpan::new(3, 7).unwrap()));
        assert_eq!(span.missing_against(2), Some(OccurrenceSpan::new(3, 7).unwrap()));
        // The anchor covers part of the span: exactly the suffix.
        assert_eq!(span.missing_against(5), Some(OccurrenceSpan::new(5, 7).unwrap()));
        // The anchor covers through hi: nothing is missing.
        assert_eq!(span.missing_against(7), None);
        assert_eq!(span.missing_against(9), None);
    }

    #[test]
    fn adjacent_and_overlapping_spans_compose_by_union() {
        let first = OccurrenceSpan::new(0, 5).unwrap();
        let second = OccurrenceSpan::new(5, 9).unwrap();
        assert_eq!(
            first.compose(&second).unwrap(),
            OccurrenceSpan::new(0, 9).unwrap()
        );
        let overlapping = OccurrenceSpan::new(3, 7).unwrap();
        assert_eq!(
            first.compose(&overlapping).unwrap(),
            OccurrenceSpan::new(0, 7).unwrap()
        );
    }

    #[test]
    fn span_gaps_fail_closed() {
        let first = OccurrenceSpan::new(0, 5).unwrap();
        let gapped = OccurrenceSpan::new(6, 9).unwrap();
        assert_eq!(first.compose(&gapped), Err(SpanError::Gap));
        assert!(!first.adjacent_or_overlaps(&gapped));
    }

    // -------------------------------------------------------- award state

    #[test]
    fn award_state_round_trips() {
        let state = AwardState {
            durable: 4,
            provisional: 5,
        };
        let bytes = state.canonical_bytes();
        assert_eq!(AwardState::decode(&bytes).unwrap(), state);
    }

    #[test]
    fn award_state_decode_rejects_malformed_input() {
        let bytes = AwardState::new().canonical_bytes();
        for cut in 0..bytes.len() {
            assert!(AwardState::decode(&bytes[..cut]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            AwardState::decode(&trailing),
            Err(SaveError::Trailing { .. })
        ));
    }

    #[test]
    fn pure_provisional_rollback_restores_earnability() {
        // The reviewer's requirement B: a provisional award discarded
        // on encounter failure must become legitimately earnable
        // again.
        let mut state = AwardState::new();
        state.award_durable(3);
        let checkpoint = state.checkpoint();
        state.award_provisional(5);
        assert_eq!(state.awarded(), 5);
        state.rollback(checkpoint);
        assert_eq!(state.awarded(), 3, "occurrence 5 is earnable again");
        assert_eq!(state.provisional, 0, "the checkpoint's pre-encounter provisional value");
    }

    #[test]
    fn durable_retention_survives_rollback_without_duplication() {
        // The reviewer's requirement B: a durable award retained
        // through encounter failure must never be duplicated.
        let mut state = AwardState::new();
        state.award_durable(3);
        let checkpoint = state.checkpoint();
        state.award_durable(5);
        state.rollback(checkpoint);
        assert_eq!(state.awarded(), 5, "the durable occurrence is retained");
        // The reattempt's eligibility check: occurrence 5 is not
        // earnable (retained).
        assert!(5 <= state.awarded());
    }

    #[test]
    fn mixed_class_interleaving_is_exact() {
        // THE scalar-watermark counterexample: one encounter retains a
        // durable occurrence 4 while rolling back a provisional
        // occurrence 5 — 4 must stay un-earnable and 5 must become
        // earnable. A single scalar cannot express this.
        let mut state = AwardState::new();
        state.award_durable(3);
        let checkpoint = state.checkpoint(); // (durable 3, provisional 3)
        state.award_durable(4); // a retained durable award during the encounter
        state.award_provisional(5); // a provisional award during the encounter
        state.rollback(checkpoint);
        // The durable contribution from during the encounter stands.
        assert_eq!(state.durable, 4);
        // The provisional contribution rolled back to the checkpoint's
        // pre-encounter value (no provisional award had been made
        // before the encounter began).
        assert_eq!(state.provisional, 0);
        // Eligibility: occurrence 4 is NOT earnable (retained);
        // occurrence 5 IS earnable (rolled back).
        assert_eq!(state.awarded(), 4);
        assert!(4 <= state.awarded());
        assert!(5 > state.awarded());
    }

    #[test]
    fn encounter_commit_folds_provisional_into_the_durable_lane() {
        let mut state = AwardState::new();
        state.award_durable(3);
        state.award_provisional(5);
        state.commit_encounter();
        assert_eq!(state.durable, 5);
        assert_eq!(state.provisional, 5);
        assert_eq!(state.awarded(), 5);
        // After commit, a failure rollback of a NEW encounter cannot
        // un-award the committed occurrences.
        let checkpoint = state.checkpoint();
        state.award_provisional(7);
        state.rollback(checkpoint);
        assert_eq!(state.awarded(), 5);
    }

    #[test]
    fn awards_never_regress() {
        let mut state = AwardState::new();
        state.award_durable(9);
        state.award_durable(4);
        assert_eq!(state.durable, 9, "a high-watermark never regresses");
        state.award_provisional(9);
        state.award_provisional(2);
        assert_eq!(state.provisional, 9);
    }
}
