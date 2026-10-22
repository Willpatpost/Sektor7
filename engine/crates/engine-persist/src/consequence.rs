//! The structured consequential identity, occurrence coverage, and
//! the class-aware award representation (§5.5, §5.4.1).
//!
//! **`ConsequenceId` (`consequence-id/1`)** — the versioned
//! structured identity replacing the Stage 4 packed `u64` key for all
//! durable pipeline state: schema `u8`, region `u32`, episode `u64`,
//! npc `u32`, fixed widths on the wire. No issued durable identity is
//! ever reused; a retry preserves its original identity; a genuinely
//! new operation receives a fresh one. The two-direction legacy
//! conversion ([`ConsequenceId::pack_stage4`] /
//! [`ConsequenceId::unpack_stage4`]) proves the mapping against the
//! actual engine-slice layout with strict bounds: a field exceeding
//! its legacy bit width is rejected, never wrapped — the unchecked
//! legacy expression would alias an out-of-range npc into the episode
//! bits (an identity collision). The legacy in-memory packed
//! key and its fingerprint domain remain untouched (B4 owns any
//! migration).
//!
//! **`OccurrenceSpan`** — the ratified `(lo, hi]` convention
//! (exclusive lower, inclusive upper): an aggregate capture under one
//! `ConsequenceId` covers the occurrences `(lo, hi]`; adjacent spans
//! compose by union; a gap or inversion fails closed; partial anchor
//! coverage leaves exactly the uncovered occurrences missing.
//!
//! **`Coverage`** — exact interval-set coverage: a sorted,
//! maximally-composed list of disjoint `(lo, hi]` spans. Holes are
//! legitimate here (a hole is a rolled-back provisional occurrence);
//! gaps fail closed only in envelope/recovery reconciliation, a
//! different context with different rules. A coverage insertion is a
//! **set operation** — never a numerical increment to a scalar
//! high-watermark.
//!
//! **`AwardState`** — the class-aware awarded-occurrence
//! representation (the v1.8 rollback table): the durable lane and
//! the encounter transaction's provisional lane as two exact
//! coverages. Rollback restores the provisional coverage wholesale
//! from the checkpoint (durable coverage never rolls back); commit
//! folds the provisional contributions into the durable lane;
//! explicit, idempotent promotion moves a span between lanes. A
//! single scalar high-watermark is **insufficient**: an encounter
//! that retains a durable occurrence 4 while rolling back a
//! provisional occurrence 5, then a durable occurrence 6, must leave
//! 4 and 6 un-earnable and 5 legitimately earnable — a scalar of 6
//! would falsely cover the hole at 5. Cross-lane overlap (the same
//! occurrence counted in both lanes) is an invalid state that
//! decoding rejects — never silently repaired.

use std::fmt;

use engine_core::{Decoder, Encoder, SaveError};

/// The structured-identity domain (versioned; the version is the wire
/// schema byte below).
pub const CONSEQUENCE_ID_DOMAIN: &str = "sektor7/engine-persist/consequence-id/1";
/// The award-state domain (versioned; never shipped before this
/// batch — the interval-set payload is the canonical form).
pub const AWARD_STATE_DOMAIN: &str = "sektor7/engine-persist/award-state/1";

pub const CONSEQUENCE_ID_SCHEMA: u8 = 1;
pub const AWARD_STATE_SCHEMA: u8 = 1;

/// Documented maximum for a persisted coverage's interval count
/// (bounded decode: each span is 16 bytes, so the count is also
/// bounded by the remaining input).
pub const MAX_COVERAGE_SPANS: u64 = 1 << 16;

/// The Stage 4 legacy packed-key layout, read firsthand from
/// `engine-slice`'s `consequence_key` (`region.rs`:
/// `(region << 54) | (episode << 10) | npc`) — never inferred from
/// design docs or NPC counts: npc occupies bits 0–9, episode bits
/// 10–53, region bits 54–63. The layout is a bijection between the
/// full `u64` range and the bounded identity domain below (the
/// all-bounds identity packs to `u64::MAX`).
pub const STAGE4_NPC_BITS: u32 = 10;
/// Episode bits in the legacy layout (bits 10–53).
pub const STAGE4_EPISODE_BITS: u32 = 44;
/// Region bits in the legacy layout (bits 54–63).
pub const STAGE4_REGION_BITS: u32 = 10;
/// The largest npc the legacy layout can express (bits 0–9).
pub const STAGE4_MAX_NPC: u32 = (1 << STAGE4_NPC_BITS) - 1;
/// The largest episode the legacy layout can express (bits 10–53).
pub const STAGE4_MAX_EPISODE: u64 = (1u64 << STAGE4_EPISODE_BITS) - 1;
/// The largest region the legacy layout can express (bits 54–63).
pub const STAGE4_MAX_REGION: u32 = (1 << STAGE4_REGION_BITS) - 1;

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

    /// Pack into the Stage 4 legacy `u64` capture-key layout (the
    /// engine-slice `consequence_key` expression — the compatibility
    /// mapping the architecture's Correction B proves by test).
    /// Strict bounds, rejection never wrap: a field exceeding its
    /// legacy bit width would silently alias into a neighboring
    /// field's bits (npc 1024 ≡ episode 1, npc 0), so it is rejected
    /// instead — an identity collision is never packed.
    pub fn pack_stage4(&self) -> Result<u64, PackedKeyError> {
        if self.npc > STAGE4_MAX_NPC {
            return Err(PackedKeyError::NpcOverflow);
        }
        if self.episode > STAGE4_MAX_EPISODE {
            return Err(PackedKeyError::EpisodeOverflow);
        }
        if self.region > STAGE4_MAX_REGION {
            return Err(PackedKeyError::RegionOverflow);
        }
        Ok((u64::from(self.region) << 54) | (self.episode << 10) | u64::from(self.npc))
    }

    /// Unpack a Stage 4 legacy `u64` capture key into the structured
    /// identity. Total: the layout is a bijection onto the bounded
    /// domain, so every `u64` decodes and this direction cannot
    /// fail; the bounds discipline lives entirely in
    /// [`Self::pack_stage4`].
    pub fn unpack_stage4(packed: u64) -> Self {
        ConsequenceId {
            region: (packed >> 54) as u32,
            episode: (packed >> 10) & STAGE4_MAX_EPISODE,
            npc: (packed & u64::from(STAGE4_MAX_NPC)) as u32,
        }
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

/// The identity does not fit the Stage 4 legacy packed-key layout: a
/// field exceeds its bit width. Rejected, never wrapped — the
/// unchecked legacy expression would alias the excess into a
/// neighboring field's bits, colliding distinct identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackedKeyError {
    /// npc exceeds bits 0–9 (would alias into the episode bits).
    NpcOverflow,
    /// episode exceeds bits 10–53 (would alias into the region
    /// bits).
    EpisodeOverflow,
    /// region exceeds bits 54–63 (would shift out of the word).
    RegionOverflow,
}

impl fmt::Display for PackedKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackedKeyError::NpcOverflow => {
                write!(f, "npc exceeds the Stage 4 packed-key width (bits 0-9)")
            }
            PackedKeyError::EpisodeOverflow => {
                write!(
                    f,
                    "episode exceeds the Stage 4 packed-key width (bits 10-53)"
                )
            }
            PackedKeyError::RegionOverflow => {
                write!(
                    f,
                    "region exceeds the Stage 4 packed-key width (bits 54-63)"
                )
            }
        }
    }
}

impl std::error::Error for PackedKeyError {}

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

    /// The occurrences still missing against a **scalar** anchor
    /// coverage (a contiguous prefix): exactly `(max(lo, A), hi]`, or
    /// `None` when nothing is missing. For hole-bearing anchors use
    /// [`Coverage::missing_of`].
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

/// Exact interval-set coverage: a sorted, maximally-composed list of
/// disjoint `(lo, hi]` occurrence spans.
///
/// **Holes are legitimate** in the award lanes — a hole is a
/// rolled-back provisional occurrence, and coverage arithmetic must
/// preserve it exactly (never treat a hole as covered). Gaps fail
/// closed only in envelope/recovery reconciliation, a different
/// context. Insertion is a **set operation**: touching spans compose,
/// gaps remain separate intervals — never a scalar maximum.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Coverage {
    /// Sorted by `lo`; disjoint and non-adjacent (maximally
    /// composed) by construction.
    spans: Vec<OccurrenceSpan>,
}

impl Coverage {
    pub fn new() -> Self {
        Coverage { spans: Vec::new() }
    }

    /// The canonical span list (sorted, disjoint, non-adjacent).
    pub fn spans(&self) -> &[OccurrenceSpan] {
        &self.spans
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Whether occurrence `k` is covered by any span.
    pub fn covers(&self, k: u64) -> bool {
        self.spans.iter().any(|span| span.covers(k))
    }

    /// Set-union insertion: every span touching or overlapping the
    /// inserted span (including adjacency — `s.hi == lo` composes)
    /// merges into one; gaps remain separate intervals. Idempotent:
    /// re-inserting covered occurrences is a no-op.
    pub fn insert_span(&mut self, span: OccurrenceSpan) {
        let mut lo = span.lo;
        let mut hi = span.hi;
        let mut merged: Vec<OccurrenceSpan> = Vec::with_capacity(self.spans.len());
        for existing in self.spans.drain(..) {
            if existing.hi >= lo && existing.lo <= hi {
                lo = lo.min(existing.lo);
                hi = hi.max(existing.hi);
            } else {
                merged.push(existing);
            }
        }
        merged.push(OccurrenceSpan { lo, hi });
        merged.sort_unstable_by_key(|span| span.lo);
        self.spans = merged;
    }

    /// Set subtraction: remove every occurrence covered by `span`
    /// (possibly splitting a span around the removed range).
    pub fn remove_span(&mut self, span: &OccurrenceSpan) {
        let mut result = Vec::with_capacity(self.spans.len() + 1);
        for existing in &self.spans {
            // Shared occurrences iff the (lo, hi] ranges intersect.
            if existing.lo < span.hi && span.lo < existing.hi {
                // Left remainder: occurrences at or below span.lo.
                let left_hi = existing.hi.min(span.lo);
                if existing.lo < left_hi {
                    result.push(OccurrenceSpan {
                        lo: existing.lo,
                        hi: left_hi,
                    });
                }
                // Right remainder: occurrences above span.hi.
                let right_lo = existing.lo.max(span.hi);
                if right_lo < existing.hi {
                    result.push(OccurrenceSpan {
                        lo: right_lo,
                        hi: existing.hi,
                    });
                }
            } else {
                result.push(*existing);
            }
        }
        self.spans = result;
    }

    /// The sub-spans of `span` whose occurrences this coverage does
    /// **not** cover — exact against hole-bearing anchors (an anchor
    /// covering {4, 6} against the span `(3, 7]` yields exactly
    /// `(4, 5]` and `(6, 7]`: occurrences 5 and 7).
    pub fn missing_of(&self, span: &OccurrenceSpan) -> Vec<OccurrenceSpan> {
        let mut missing = Vec::new();
        let mut cursor = span.lo;
        for existing in &self.spans {
            if existing.hi <= cursor {
                continue;
            }
            if existing.lo >= span.hi {
                break;
            }
            let gap_hi = existing.lo.min(span.hi);
            if cursor < gap_hi {
                missing.push(OccurrenceSpan {
                    lo: cursor,
                    hi: gap_hi,
                });
            }
            cursor = cursor.max(existing.hi);
            if cursor >= span.hi {
                break;
            }
        }
        if cursor < span.hi {
            missing.push(OccurrenceSpan {
                lo: cursor,
                hi: span.hi,
            });
        }
        missing
    }

    /// Whether the two coverages share any occurrence (cross-lane
    /// overlap — an invalid award state).
    pub fn overlaps(&self, other: &Coverage) -> bool {
        self.spans
            .iter()
            .any(|a| other.spans.iter().any(|b| a.lo < b.hi && b.lo < a.hi))
    }

    /// Wire encoding (bounded: the interval count is checked against
    /// the documented maximum before serializing). Crate-internal:
    /// the award-state domain and the envelope's source baseline
    /// embed the same span-list form.
    pub(crate) fn encode_into(&self, enc: &mut Encoder) -> Result<(), SaveError> {
        if self.spans.len() as u64 > MAX_COVERAGE_SPANS {
            return Err(SaveError::Limit {
                section: "award-state",
                kind: "coverage-span-count",
                value: self.spans.len() as u64,
                max: MAX_COVERAGE_SPANS,
            });
        }
        enc.write_count(self.spans.len());
        for span in &self.spans {
            enc.write_u64(span.lo);
            enc.write_u64(span.hi);
        }
        Ok(())
    }

    /// Strict decode from a decoder stream (fail-closed: bounded
    /// count — also bounded by the remaining input — per-span
    /// validity, sorted, disjoint, and **non-adjacent** canonical
    /// form; adjacent spans would have composed and are therefore
    /// non-canonical). Crate-internal: shared with the envelope's
    /// source-baseline decoding.
    pub(crate) fn decode_from(dec: &mut Decoder<'_>) -> Result<Self, SaveError> {
        const SECTION: &str = "award-state";
        let count = dec.read_count(MAX_COVERAGE_SPANS, SECTION, "coverage-span-count")?;
        // Each encoded span is exactly 16 bytes.
        let remaining = dec.remaining() as u64;
        if count > remaining / 16 {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "coverage-span-count",
                value: count,
                max: remaining / 16,
            });
        }
        let mut spans: Vec<OccurrenceSpan> = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let lo = dec.read_u64()?;
            let hi = dec.read_u64()?;
            if lo >= hi {
                return Err(SaveError::Integrity("award-state/coverage-span-inverted"));
            }
            if let Some(prev) = spans.last()
                && lo <= prev.hi
            {
                // Overlapping or adjacent: non-canonical (the
                // set-union insertion composes touching spans).
                return Err(SaveError::Integrity(
                    "award-state/coverage-span-noncanonical",
                ));
            }
            spans.push(OccurrenceSpan { lo, hi });
        }
        Ok(Coverage { spans })
    }
}

/// The pre-encounter award-state snapshot — what a rollback restores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwardCheckpoint {
    pub durable: Coverage,
    pub provisional: Coverage,
}

/// The class-aware awarded-occurrence state for one NPC: the durable
/// lane's coverage and the encounter transaction's provisional
/// coverage, as two exact interval sets.
///
/// **Why two lanes (the v1.8 rollback table):** a `DurableEligible`
/// award's coverage lives in the durable lane — outside the
/// encounter transaction — and is **never rolled back** (a
/// rollback-then-reattempt cycle cannot duplicate it); an
/// `EncounterProvisional` award's coverage lives in the encounter
/// transaction and **rolls back with it** (the checkpoint restores
/// the pre-encounter coverage, so the legitimate reattempt can earn
/// it again). Neither lane is a scalar: a hole between retained
/// durable occurrences is a rolled-back provisional occurrence, and
/// exact interval membership preserves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwardState {
    /// Durable-lane coverage (journal envelopes / snapshots / the
    /// confirmed chain) — set-only growth, never rolled back.
    pub durable: Coverage,
    /// The current encounter transaction's provisional coverage —
    /// restorable wholesale from the encounter checkpoint.
    pub provisional: Coverage,
}

impl AwardState {
    pub fn new() -> Self {
        AwardState {
            durable: Coverage::new(),
            provisional: Coverage::new(),
        }
    }

    /// Union-membership eligibility: occurrence `k` is awarded iff
    /// **either** lane covers it (interval membership — never a
    /// scalar comparison).
    pub fn covers(&self, k: u64) -> bool {
        self.durable.covers(k) || self.provisional.covers(k)
    }

    /// A durability-classified award at occurrence `k` (occurrences
    /// number from 1): inserts `(k−1, k]` into the durable lane — a
    /// **set operation**. Idempotent: an already-covered occurrence
    /// (either lane) is a no-op, never a second award.
    pub fn award_durable(&mut self, k: u64) {
        if k == 0 || self.covers(k) {
            return;
        }
        self.durable
            .insert_span(OccurrenceSpan { lo: k - 1, hi: k });
    }

    /// An encounter-provisional award at occurrence `k`: inserts
    /// `(k−1, k]` into the encounter transaction's lane. Idempotent
    /// against either lane — the same occurrence can never count as
    /// two awards.
    pub fn award_provisional(&mut self, k: u64) {
        if k == 0 || self.covers(k) {
            return;
        }
        self.provisional
            .insert_span(OccurrenceSpan { lo: k - 1, hi: k });
    }

    /// Aggregate durable-lane restoration (envelope recovery): the
    /// covered span enters the durable lane as a set union.
    pub fn apply_durable_span(&mut self, span: OccurrenceSpan) {
        self.durable.insert_span(span);
    }

    /// Aggregate provisional-lane application.
    pub fn apply_provisional_span(&mut self, span: OccurrenceSpan) {
        self.provisional.insert_span(span);
    }

    /// Explicit, idempotent provisional→durable promotion: the span's
    /// occurrences join the durable lane and leave the provisional
    /// lane — identity and coverage preserved consistently; promoting
    /// an already-durable span is a no-op.
    pub fn promote_provisional(&mut self, span: &OccurrenceSpan) {
        self.durable.insert_span(*span);
        self.provisional.remove_span(span);
    }

    /// The pre-encounter checkpoint a rollback restores to.
    pub fn checkpoint(&self) -> AwardCheckpoint {
        AwardCheckpoint {
            durable: self.durable.clone(),
            provisional: self.provisional.clone(),
        }
    }

    /// Encounter-failure rollback: restores the **provisional**
    /// coverage from the checkpoint — minus any occurrences the
    /// durable lane has covered since (a durable promotion survives
    /// per its classification; restoring it to the provisional lane
    /// as well would place one occurrence in both lanes). The
    /// **durable** lane is never rolled back (durable-classified
    /// awards from during the encounter are retained — their
    /// eligibility protection stands).
    pub fn rollback(&mut self, checkpoint: AwardCheckpoint) {
        let mut restored = checkpoint.provisional;
        for span in self.durable.spans().to_vec() {
            restored.remove_span(&span);
        }
        self.provisional = restored;
    }

    /// Encounter commit: the transaction's provisional contributions
    /// are committed through their durability path and fold into the
    /// durable lane (set union); the provisional lane empties.
    pub fn commit_encounter(&mut self) {
        for span in self.provisional.spans().to_vec() {
            self.durable.insert_span(span);
        }
        self.provisional = Coverage::new();
    }

    /// Cross-lane coherence: the same occurrence in both lanes is an
    /// invalid state (never two awards) — rejected, never repaired.
    pub fn validate(&self) -> Result<(), SaveError> {
        if self.durable.overlaps(&self.provisional) {
            return Err(SaveError::Integrity("award-state/cross-lane-overlap"));
        }
        Ok(())
    }

    /// Canonical bytes (versioned; bounded interval counts; only
    /// cross-lane-coherent states serialize).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SaveError> {
        self.validate()?;
        let mut enc = Encoder::new();
        enc.write_str(AWARD_STATE_DOMAIN);
        enc.write_u8(AWARD_STATE_SCHEMA);
        self.durable.encode_into(&mut enc)?;
        self.provisional.encode_into(&mut enc)?;
        Ok(enc.into_bytes())
    }

    /// Strict decode (fail-closed; cross-lane overlap rejected).
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
        let durable = Coverage::decode_from(&mut dec)?;
        let provisional = Coverage::decode_from(&mut dec)?;
        dec.finish()?;
        let state = AwardState {
            durable,
            provisional,
        };
        state.validate()?;
        Ok(state)
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

    fn spans_of(coverage: &Coverage) -> Vec<(u64, u64)> {
        coverage
            .spans()
            .iter()
            .map(|span| (span.lo, span.hi))
            .collect()
    }

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
        assert_ne!(
            a.canonical_bytes(),
            ConsequenceId::new(2, 2, 3).canonical_bytes()
        );
        assert_ne!(
            a.canonical_bytes(),
            ConsequenceId::new(1, 3, 3).canonical_bytes()
        );
        assert_ne!(
            a.canonical_bytes(),
            ConsequenceId::new(1, 2, 4).canonical_bytes()
        );
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
    fn missing_against_scalar_anchor_is_the_suffix() {
        let span = OccurrenceSpan::new(3, 7).unwrap();
        assert_eq!(
            span.missing_against(0),
            Some(OccurrenceSpan::new(3, 7).unwrap())
        );
        assert_eq!(
            span.missing_against(5),
            Some(OccurrenceSpan::new(5, 7).unwrap())
        );
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

    // ------------------------------------------------------------ coverage

    #[test]
    fn coverage_insert_composes_touching_spans() {
        let mut coverage = Coverage::new();
        coverage.insert_span(OccurrenceSpan::new(0, 5).unwrap());
        coverage.insert_span(OccurrenceSpan::new(5, 9).unwrap());
        assert_eq!(spans_of(&coverage), vec![(0, 9)]);
        coverage.insert_span(OccurrenceSpan::new(3, 7).unwrap());
        assert_eq!(spans_of(&coverage), vec![(0, 9)], "re-insert is a no-op");
    }

    #[test]
    fn coverage_insert_preserves_gaps_as_separate_spans() {
        let mut coverage = Coverage::new();
        coverage.insert_span(OccurrenceSpan::new(3, 4).unwrap()); // occurrence 4
        coverage.insert_span(OccurrenceSpan::new(5, 6).unwrap()); // occurrence 6
        assert_eq!(
            spans_of(&coverage),
            vec![(3, 4), (5, 6)],
            "the hole at occurrence 5 is preserved"
        );
        assert!(coverage.covers(4));
        assert!(!coverage.covers(5), "the hole is not covered");
        assert!(coverage.covers(6));
    }

    #[test]
    fn coverage_missing_of_is_exact_on_hole_bearing_anchors() {
        // The reviewer's example: an anchor covering {4, 6} against
        // the envelope span (3, 7] leaves exactly {5, 7}.
        let mut anchor = Coverage::new();
        anchor.insert_span(OccurrenceSpan::new(3, 4).unwrap());
        anchor.insert_span(OccurrenceSpan::new(5, 6).unwrap());
        let envelope = OccurrenceSpan::new(3, 7).unwrap();
        let missing = anchor.missing_of(&envelope);
        assert_eq!(
            spans_of(&Coverage { spans: missing }),
            vec![(4, 5), (6, 7)],
            "exactly occurrences 5 and 7 are missing"
        );
        // A contiguous anchor: the plain suffix.
        let mut contiguous = Coverage::new();
        contiguous.insert_span(OccurrenceSpan::new(0, 4).unwrap());
        let missing = contiguous.missing_of(&envelope);
        assert_eq!(spans_of(&Coverage { spans: missing }), vec![(4, 7)]);
        // An empty anchor: the whole span.
        let missing = Coverage::new().missing_of(&envelope);
        assert_eq!(spans_of(&Coverage { spans: missing }), vec![(3, 7)]);
        // A fully covering anchor: nothing.
        let mut full = Coverage::new();
        full.insert_span(OccurrenceSpan::new(0, 7).unwrap());
        assert!(full.missing_of(&envelope).is_empty());
    }

    #[test]
    fn coverage_remove_span_subtracts_and_splits() {
        let mut coverage = Coverage::new();
        coverage.insert_span(OccurrenceSpan::new(3, 8).unwrap());
        coverage.remove_span(&OccurrenceSpan::new(4, 6).unwrap());
        assert_eq!(
            spans_of(&coverage),
            vec![(3, 4), (6, 8)],
            "occurrences 5 and 6 removed; 4, 7, 8 retained"
        );
        // Removing beyond the coverage is a no-op.
        coverage.remove_span(&OccurrenceSpan::new(20, 30).unwrap());
        assert_eq!(spans_of(&coverage), vec![(3, 4), (6, 8)]);
    }

    #[test]
    fn coverage_round_trips_and_decode_rejects_noncanonical_forms() {
        let mut coverage = Coverage::new();
        coverage.insert_span(OccurrenceSpan::new(3, 4).unwrap());
        coverage.insert_span(OccurrenceSpan::new(6, 9).unwrap());
        let mut enc = Encoder::new();
        coverage.encode_into(&mut enc).unwrap();
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        let decoded = Coverage::decode_from(&mut dec).unwrap();
        assert_eq!(decoded, coverage);

        // A count far beyond the remaining bytes is rejected before
        // allocation.
        let mut enc = Encoder::new();
        enc.write_count(1 << 20);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(matches!(
            Coverage::decode_from(&mut dec),
            Err(SaveError::Limit { .. })
        ));

        // An inverted span is rejected.
        let mut enc = Encoder::new();
        enc.write_count(1);
        enc.write_u64(9);
        enc.write_u64(3);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(Coverage::decode_from(&mut dec).is_err());

        // Adjacent (non-composed) spans are non-canonical.
        let mut enc = Encoder::new();
        enc.write_count(2);
        enc.write_u64(3);
        enc.write_u64(5);
        enc.write_u64(5);
        enc.write_u64(9);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(Coverage::decode_from(&mut dec).is_err());

        // Unsorted spans are non-canonical.
        let mut enc = Encoder::new();
        enc.write_count(2);
        enc.write_u64(6);
        enc.write_u64(9);
        enc.write_u64(3);
        enc.write_u64(4);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(Coverage::decode_from(&mut dec).is_err());
    }

    // ---------------------------------------------------------- award state

    fn round_trip(state: &AwardState) -> AwardState {
        let bytes = state.canonical_bytes().unwrap();
        AwardState::decode(&bytes).unwrap()
    }

    #[test]
    fn award_state_round_trips_repeatedly_and_byte_identically() {
        let mut state = AwardState::new();
        state.award_durable(4);
        state.award_durable(6);
        state.award_provisional(9);
        let first = state.canonical_bytes().unwrap();
        for _ in 0..4 {
            let restored = round_trip(&state);
            assert_eq!(restored, state);
            assert_eq!(restored.canonical_bytes().unwrap(), first);
        }
    }

    #[test]
    fn award_state_decode_rejects_malformed_input() {
        let state = AwardState::new();
        let bytes = state.canonical_bytes().unwrap();
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
    fn award_state_decode_rejects_cross_lane_overlap() {
        // Hand-encode an invalid state: the durable lane covers
        // (3, 5] while the provisional lane covers (4, 6] — the
        // shared occurrence 5 must be rejected, never repaired.
        let mut enc = Encoder::new();
        enc.write_str(AWARD_STATE_DOMAIN);
        enc.write_u8(AWARD_STATE_SCHEMA);
        enc.write_count(1);
        enc.write_u64(3);
        enc.write_u64(5);
        enc.write_count(1);
        enc.write_u64(4);
        enc.write_u64(6);
        assert!(matches!(
            AwardState::decode(&enc.into_bytes()),
            Err(SaveError::Integrity(_))
        ));
    }

    #[test]
    fn mixed_class_hole_interleaving_is_exact() {
        // THE scalar counterexample, in its strongest form: durable
        // 4, provisional 5, durable 6 — rollback leaves {4, 6}
        // retained with 5 legitimately earnable again (a scalar
        // high-watermark of 6 would falsely cover the hole at 5).
        let mut state = AwardState::new();
        state.award_durable(3);
        let checkpoint = state.checkpoint();
        state.award_durable(4);
        state.award_provisional(5);
        state.award_durable(6);
        state.rollback(checkpoint);
        assert!(state.covers(4), "the retained durable occurrence 4");
        assert!(state.covers(6), "the retained durable occurrence 6");
        assert!(
            !state.covers(5),
            "the rolled-back provisional occurrence 5 is earnable again"
        );
        // The reattempt legitimately re-earns occurrence 5 — and the
        // hole fills, composing back to one span.
        state.award_durable(5);
        assert!(state.covers(5));
        assert_eq!(spans_of(&state.durable), vec![(2, 6)]);
    }

    #[test]
    fn alternating_classes_rollback_exactly() {
        let mut state = AwardState::new();
        state.award_durable(1);
        let checkpoint = state.checkpoint();
        state.award_provisional(2);
        state.award_durable(3);
        state.award_provisional(4);
        state.award_durable(5);
        state.rollback(checkpoint);
        for retained in [1, 3, 5] {
            assert!(
                state.covers(retained),
                "durable occurrence {retained} is retained"
            );
        }
        for discarded in [2, 4] {
            assert!(
                !state.covers(discarded),
                "provisional occurrence {discarded} is earnable again"
            );
        }
    }

    #[test]
    fn mixed_class_aggregate_spans_reconcile_exactly() {
        // Aggregate envelope restoration into the durable lane plus a
        // provisional aggregate during the encounter.
        let mut state = AwardState::new();
        state.apply_durable_span(OccurrenceSpan::new(3, 6).unwrap()); // occurrences 4,5,6
        let checkpoint = state.checkpoint();
        state.apply_provisional_span(OccurrenceSpan::new(5, 8).unwrap()); // occurrences 6,7,8
        state.rollback(checkpoint);
        for retained in [4, 5, 6] {
            assert!(state.covers(retained));
        }
        for discarded in [7, 8] {
            assert!(
                !state.covers(discarded),
                "occurrence {discarded} is earnable again"
            );
        }
    }

    #[test]
    fn pure_provisional_rollback_restores_earnability() {
        let mut state = AwardState::new();
        state.award_durable(3);
        let checkpoint = state.checkpoint();
        state.award_provisional(5);
        assert!(state.covers(5));
        state.rollback(checkpoint);
        assert!(!state.covers(5), "occurrence 5 is earnable again");
        assert!(state.covers(3), "the earlier durable award stands");
    }

    #[test]
    fn durable_retention_survives_rollback_without_duplication() {
        let mut state = AwardState::new();
        state.award_durable(3);
        let checkpoint = state.checkpoint();
        state.award_durable(5);
        state.rollback(checkpoint);
        assert!(state.covers(5), "the durable occurrence is retained");
        // A reattempt double-award attempt is a no-op, never a second
        // award.
        state.award_provisional(5);
        assert!(state.covers(5));
        assert!(
            state.validate().is_ok(),
            "the same occurrence never occupies both lanes"
        );
    }

    #[test]
    fn double_award_attempts_are_idempotent_no_ops() {
        let mut state = AwardState::new();
        state.award_durable(5);
        state.award_durable(5);
        assert_eq!(spans_of(&state.durable), vec![(4, 5)]);
        state.award_provisional(5);
        assert!(
            state.provisional.is_empty(),
            "an already-durable occurrence never enters the provisional lane"
        );
        assert!(state.validate().is_ok());
    }

    #[test]
    fn promotion_is_explicit_and_idempotent() {
        let mut state = AwardState::new();
        state.award_provisional(5);
        state.award_provisional(6);
        let span = OccurrenceSpan::new(4, 6).unwrap();
        state.promote_provisional(&span);
        assert!(state.durable.covers(5) && state.durable.covers(6));
        assert!(
            state.provisional.is_empty(),
            "promoted occurrences leave the provisional lane"
        );
        assert!(state.validate().is_ok());
        // Idempotent re-promotion.
        state.promote_provisional(&span);
        assert!(state.durable.covers(5) && state.durable.covers(6));
        assert!(state.provisional.is_empty());
        // A rollback after promotion cannot un-award the promoted
        // occurrences (the checkpoint predates them; the durable lane
        // never rolls back).
        let checkpoint = state.checkpoint();
        state.award_provisional(7);
        state.rollback(checkpoint);
        assert!(state.covers(5) && state.covers(6));
        assert!(!state.covers(7));
    }

    #[test]
    fn promotion_survives_rollback_without_cross_lane_overlap() {
        // The reviewer's targeted regression: the checkpoint's
        // provisional coverage contains occurrence 5; 5 is promoted
        // to durable; the encounter fails — restoring the checkpoint
        // must not place 5 back into the provisional lane alongside
        // its durable promotion (one occurrence, two lanes).
        let mut state = AwardState::new();
        state.award_provisional(5);
        let checkpoint = state.checkpoint();
        state.promote_provisional(&OccurrenceSpan::new(4, 5).unwrap());
        assert!(state.durable.covers(5));
        assert!(state.provisional.is_empty());
        state.rollback(checkpoint);
        assert!(
            state.durable.covers(5),
            "the durable promotion survives per its classification"
        );
        assert!(
            !state.provisional.covers(5),
            "rollback never restores a promoted occurrence to the provisional lane"
        );
        assert!(state.validate().is_ok());
        // The restored state serializes (cross-lane coherent).
        assert!(state.canonical_bytes().is_ok());
    }

    #[test]
    fn partial_promotion_rollback_restores_only_unpromoted_coverage() {
        // A mixed checkpoint: provisional {5, 7}; only 5 is promoted;
        // rollback restores provisional {7} with 5 durable-only.
        let mut state = AwardState::new();
        state.award_provisional(5);
        state.award_provisional(7);
        let checkpoint = state.checkpoint();
        state.promote_provisional(&OccurrenceSpan::new(4, 5).unwrap());
        state.rollback(checkpoint);
        assert!(state.durable.covers(5));
        assert!(!state.provisional.covers(5));
        assert!(
            state.provisional.covers(7),
            "the unpromoted provisional occurrence is restored and earnable-on-reattempt"
        );
        assert!(state.validate().is_ok());
    }

    #[test]
    fn encounter_commit_folds_provisional_into_the_durable_lane() {
        let mut state = AwardState::new();
        state.award_durable(3);
        state.award_provisional(5);
        state.commit_encounter();
        assert!(state.durable.covers(5));
        assert!(state.provisional.is_empty());
        assert!(state.covers(5));
        // After commit, a new encounter's rollback cannot un-award
        // the committed occurrences.
        let checkpoint = state.checkpoint();
        state.award_provisional(7);
        state.rollback(checkpoint);
        assert!(state.covers(5));
        assert!(!state.covers(7));
    }

    #[test]
    fn occurrence_zero_is_never_awarded() {
        let mut state = AwardState::new();
        state.award_durable(0);
        state.award_provisional(0);
        assert!(state.durable.is_empty());
        assert!(state.provisional.is_empty());
    }

    #[test]
    fn stage4_packing_matches_the_legacy_layout_exactly() {
        // The legacy expression, read firsthand from engine-slice's
        // consequence_key (region.rs) — engine-persist cannot depend
        // on engine-slice (the ratified dependency direction), so the
        // compatibility proof mirrors the expression and pins the
        // exact literals it produces.
        let legacy = |region: u32, episode: u64, npc: u32| {
            (u64::from(region) << 54) | (episode << 10) | u64::from(npc)
        };
        let cases = [
            (0u32, 0u64, 0u32),
            (0, 0, 1),
            (0, 1, 0),
            (1, 0, 0),
            (4, 0, 0),
            (7, 0, 1023),
            (0, STAGE4_MAX_EPISODE, 0),
            (STAGE4_MAX_REGION, STAGE4_MAX_EPISODE, STAGE4_MAX_NPC),
        ];
        for (region, episode, npc) in cases {
            let id = ConsequenceId::new(region, episode, npc);
            assert_eq!(id.pack_stage4().unwrap(), legacy(region, episode, npc));
        }
        // Exact literals pin the bit layout independent of any
        // re-derivation.
        assert_eq!(ConsequenceId::new(0, 1, 0).pack_stage4().unwrap(), 1 << 10);
        assert_eq!(ConsequenceId::new(1, 0, 0).pack_stage4().unwrap(), 1 << 54);
        assert_eq!(
            ConsequenceId::new(STAGE4_MAX_REGION, STAGE4_MAX_EPISODE, STAGE4_MAX_NPC)
                .pack_stage4()
                .unwrap(),
            u64::MAX,
            "the all-bounds identity is u64::MAX — the layout is a bijection onto u64"
        );
    }

    #[test]
    fn stage4_packing_round_trips_over_the_full_layout() {
        // pack∘unpack is the identity on every u64 (the layout is a
        // bijection); unpack∘pack is the identity on every in-bounds
        // identity.
        let patterns = [
            0u64,
            1,
            u64::MAX,
            0x3FF,
            0x400,
            (1u64 << 54) - 1,
            1u64 << 54,
            0xAAAA_AAAA_AAAA_AAAA,
            0x5555_5555_5555_5555,
            0x1C00_0000_0000_03FF,
        ];
        for packed in patterns {
            let id = ConsequenceId::unpack_stage4(packed);
            assert_eq!(id.pack_stage4().unwrap(), packed);
        }
        for (region, episode, npc) in [
            (0u32, 0u64, 0u32),
            (7, 3, 999),
            (STAGE4_MAX_REGION, STAGE4_MAX_EPISODE, STAGE4_MAX_NPC),
        ] {
            let id = ConsequenceId::new(region, episode, npc);
            assert_eq!(ConsequenceId::unpack_stage4(id.pack_stage4().unwrap()), id);
        }
        // Known field boundaries.
        assert_eq!(ConsequenceId::unpack_stage4(0), ConsequenceId::new(0, 0, 0));
        assert_eq!(
            ConsequenceId::unpack_stage4(1 << 10),
            ConsequenceId::new(0, 1, 0)
        );
        assert_eq!(
            ConsequenceId::unpack_stage4(1 << 54),
            ConsequenceId::new(1, 0, 0)
        );
        assert_eq!(
            ConsequenceId::unpack_stage4(u64::MAX),
            ConsequenceId::new(STAGE4_MAX_REGION, STAGE4_MAX_EPISODE, STAGE4_MAX_NPC)
        );
    }

    #[test]
    fn stage4_packing_rejects_overflow_never_wraps() {
        // The exact aliasing the strict bounds exist to prevent: the
        // unchecked legacy expression packs npc=1024 and the distinct
        // identity (episode=1, npc=0) to the SAME u64. The conversion
        // rejects the out-of-range field instead of packing a
        // collision.
        let unchecked = |region: u32, episode: u64, npc: u32| {
            (u64::from(region) << 54) | (episode << 10) | u64::from(npc)
        };
        assert_eq!(unchecked(0, 0, 1024), unchecked(0, 1, 0));
        assert_eq!(
            ConsequenceId::new(0, 0, 1024).pack_stage4(),
            Err(PackedKeyError::NpcOverflow)
        );
        // One past each boundary, and the full-width excesses.
        assert_eq!(
            ConsequenceId::new(0, 0, STAGE4_MAX_NPC + 1).pack_stage4(),
            Err(PackedKeyError::NpcOverflow)
        );
        assert_eq!(
            ConsequenceId::new(0, 0, u32::MAX).pack_stage4(),
            Err(PackedKeyError::NpcOverflow)
        );
        assert_eq!(
            ConsequenceId::new(0, STAGE4_MAX_EPISODE + 1, 0).pack_stage4(),
            Err(PackedKeyError::EpisodeOverflow)
        );
        assert_eq!(
            ConsequenceId::new(0, u64::MAX, 0).pack_stage4(),
            Err(PackedKeyError::EpisodeOverflow)
        );
        assert_eq!(
            ConsequenceId::new(STAGE4_MAX_REGION + 1, 0, 0).pack_stage4(),
            Err(PackedKeyError::RegionOverflow)
        );
        assert_eq!(
            ConsequenceId::new(u32::MAX, 0, 0).pack_stage4(),
            Err(PackedKeyError::RegionOverflow)
        );
    }
}
