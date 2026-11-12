//! The C5a head-relative Range-terminal installation OBSERVATION
//! verifier (V2X-CP3-B-P2A-C5a — the owner's ratified C5a GO,
//! delivered as a read-only observation verifier only).
//!
//! **The C5a boundary (the owner's authorization):** this module
//! implements ONE bounded, reader-generic, read-only walk that
//! determines whether a V2 `/3` Range-terminal marker and its
//! supporting retained evidence are coherently OBSERVED on the
//! revision ancestry reachable from a CALLER-SUPPLIED HEAD —
//! NOTHING ELSE. This establishes **head-relative observation**,
//! never provider-canonical installation. It must not and does
//! not confer: provider canonical-head authority; begin
//! durability authority; historical retirement correctness;
//! physical deletion completion; journal retirement or ownership
//! completion; checkpoint-crossing permission; startup-safety
//! permission; or any destructive capability. The positive result
//! carries an explicit [`UnresolvedObligations`] list naming every
//! one of those non-established authorities.
//!
//! **The no-skip discipline (the owner's ratified refinement):**
//! the walk NEVER consults checkpoint skip tables and NEVER
//! traverses through missing ancestry. A missing parent
//! classifies as [`RangeInstallationObservation::AncestryGapUnresolved`]
//! — possibly a legitimately retired intervening range whose
//! crossing belongs to the separately authorized
//! logical-retirement-aware traversal — never an invented skip,
//! never a silent absence. A DIFFERENT checkpoint boundary
//! encountered before the requested observation completes
//! classifies as [`RangeInstallationObservation::HiddenBehindCheckpointBoundary`]
//! — the marker may exist below it, so absence is NOT claimed. The
//! walk stops AT the requested covering MK (never following its
//! parent into the potentially reclaimed range) and at the
//! founding. [`RangeInstallationObservation::NotObservedOnAncestry`]
//! is returned ONLY after a complete, uninterrupted, bounded walk
//! over the relevant search segment — terminated by the founding
//! or by the EXPECTED covering checkpoint itself — and never
//! because of a missing parent, an exhausted bound, an uncrossable
//! boundary, corrupt evidence, a failed read, or an
//! unauthenticated covering MK.
//!
//! **The temporal ordering (the owner's ratified refinement):**
//! the walk enforces the publication order on the inspected
//! segment — the MK precedes the plan; the plan and every
//! plan-defined audit precede the begin; each `/2` Batch marker
//! follows the begin and its associated audit; the `/3` Range
//! terminal follows every Batch marker it claims to aggregate.
//! Unrelated ordinary publications may interleave when otherwise
//! valid. Evidence appearing ABOVE the terminal, a Batch marker
//! BELOW the begin, an audit ABOVE the begin, or a terminal whose
//! required Batch evidence is missing all refuse typed — evidence
//! appearing later on the chain never satisfies an earlier
//! terminal's claim. Every Batch-marker identity used for coverage
//! was ENCOUNTERED on the inspected ancestry, never merely
//! retrieved by content hash from off-chain storage.
//!
//! **The retained-evidence consistency layer (the owner's ratified
//! refinement):** after the covering MK is reached, the walk
//! verifies — as RETAINED-EVIDENCE consistency only, never
//! historical correctness — the MK's exact deterministic
//! checkpoint key, its single-content-object shape, its strictly
//! decoded metadata, its index shape and uniqueness, and its
//! SK-parent binding; the observed V2 plan's exact deterministic
//! key, its MK/SK/digest/seed bindings, its admitted
//! ledger-byte ceiling, and its flattened membership
//! position-by-position against the MK index's epoch-descending
//! retirement membership; every plan-defined audit's exact
//! deterministic key, hash-verified revision and object, its
//! admitted byte limit enforced BEFORE decoding, its strictly
//! decoded V2 ledger payload, its MK/SK/batch/digest/seed
//! bindings, its exact ordered unit-record membership, and its
//! `reclaimed_keys` RECOMPUTED from the retained index's
//! keyed-member information; and the observed begin's payload
//! bindings to the observed plan. The existing
//! founding-descending begin-authority verifier is NEVER invoked.
//!
//! **The commitment boundary (C3's ratified decision, preserved):**
//! the coverage and journal-obligation commitments are rederived
//! ONLY from evidence observed on the inspected segment — the
//! observed `/2` marker identities and counters through the
//! accepted C2 structural reader with contexts assembled from the
//! OBSERVED facts, and the observed plan's journal filenames —
//! and compared against the terminal's claims. Neither successful
//! computation establishes physical retirement: the Batch
//! markers' classification commitments remain historical claims
//! unless a separately authorized verifier authenticates their
//! underlying classifications.
//!
//! **The authority boundary:** a positive observation establishes
//! that a hash-valid, structurally valid, correctly keyed,
//! context-matching `/3` terminal — together with its covering
//! MK, plan, begin, audits, and Batch markers — was encountered on
//! the ancestry reachable from the supplied head, with the
//! segment's retained evidence mutually consistent and both
//! commitments in agreement with the observed evidence. It does
//! NOT establish that the supplied head is the provider's
//! canonical HEAD (a later, C5b provider-coordinated concern),
//! that the original range's history below the MK was correct, or
//! that ANY retirement obligation was discharged.

use crate::batch_completion::{
    BatchCompletionContext, RECLAMATION_COMPLETE_V2_KEY_DOMAIN, read_structural_batch_candidate,
};
use crate::checkpoint::{CheckpointMetadata, checkpoint_mk_idempotency_key};
use crate::hash::{Hash, hash_object};
use crate::medium::MediumError;
use crate::range_completion::{
    RangeCompletionContext, RangeCoverageObservation, StructuralRangeCandidate,
    read_structural_range_candidate,
};
use crate::reclaim::{
    MAX_RECLAMATION_LEDGER_BYTES, PlanBatch, RECLAMATION_COMPLETE_KEY_DOMAIN,
    RECLAMATION_LEDGER_V2_KEY_DOMAIN, ReclaimPlan, ReclamationBegin, ReclamationLedgerV2,
    reclamation_begin_idempotency_key, reclamation_ledger_idempotency_key,
    reclamation_plan_idempotency_key,
};
use crate::record::{RecordKind, RevisionRecord};

/// The checkpoint-MK key family prefix (the boundary records).
const CHECKPOINT_MK_KEY_PREFIX: &str = "engine-persist/checkpoint-mk/";
/// The completion-marker key family prefix (V1 `/1`, V2 `/2`, V3
/// `/3` — every other version is an unsupported variant).
const COMPLETION_KEY_FAMILY: &str = "engine-persist/reclamation-complete-key/";
/// The begin key family prefix (only `/1` is ratified).
const BEGIN_KEY_FAMILY: &str = "engine-persist/reclamation-begin-key/";
/// The plan key family prefix (only `/1` is ratified).
const PLAN_KEY_FAMILY: &str = "engine-persist/reclamation-plan-key/";
/// The reclamation-ledger key family prefix (V1 `/1` and V2 `/2`).
const LEDGER_KEY_FAMILY: &str = "engine-persist/reclamation-ledger-key/";

/// The default traversal ceiling — the same bound every accepted
/// descent uses.
pub const MAX_OBSERVATION_RECORDS: u64 = 1 << 20;

// ------------------------------------------------------------- the limits

/// The caller-tightenable observation traversal limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeInstallationLimits {
    /// The maximum number of ancestry records consulted by ONE
    /// observation walk. Must be at least one; the default is
    /// [`MAX_OBSERVATION_RECORDS`] (2^20).
    pub max_records: u64,
}

impl Default for RangeInstallationLimits {
    fn default() -> Self {
        RangeInstallationLimits {
            max_records: MAX_OBSERVATION_RECORDS,
        }
    }
}

impl RangeInstallationLimits {
    /// The pure validation: a zero limit never consults anything
    /// and is refused before any read.
    pub fn validate(&self) -> Result<(), MediumError> {
        if self.max_records == 0 {
            return Err(MediumError::Chain {
                rule: "range-observation/zero-limit",
            });
        }
        Ok(())
    }
}

// --------------------------------------------------- the unresolved obligations

/// The explicit not-established marker — a NAMED ABSENCE of
/// authority, never a capability, never a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotEstablished;

/// The authorities a positive observation explicitly does NOT
/// establish — every field is a named absence, reserved for the
/// separately authorized checkpoints that may one day establish
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnresolvedObligations {
    /// The provider-coordinated canonical HEAD (C5b).
    pub provider_canonical_head: NotEstablished,
    /// The begin's provider durability authority.
    pub begin_durability_authority: NotEstablished,
    /// The founding-derived seed's authenticity (below the MK).
    pub founding_derived_seed_authenticity: NotEstablished,
    /// The original range digest's historical correctness.
    pub original_range_digest_historical_correctness: NotEstablished,
    /// The original object closures below the MK.
    pub original_object_closures: NotEstablished,
    /// The Batch markers' historical classification correctness.
    pub batch_classification_historical_correctness: NotEstablished,
    /// The physical retirement of any identity.
    pub physical_retirement: NotEstablished,
    /// The journal retirement and ownership updates.
    pub journal_retirement_and_ownership: NotEstablished,
    /// The recovery convergence of any range.
    pub recovery_convergence: NotEstablished,
    /// Any checkpoint-crossing permission.
    pub crossing_permission: NotEstablished,
    /// Any startup-safety permission.
    pub startup_safety: NotEstablished,
}

impl UnresolvedObligations {
    /// The complete list — a positive observation carries every
    /// not-established authority explicitly.
    pub fn all() -> Self {
        UnresolvedObligations {
            provider_canonical_head: NotEstablished,
            begin_durability_authority: NotEstablished,
            founding_derived_seed_authenticity: NotEstablished,
            original_range_digest_historical_correctness: NotEstablished,
            original_object_closures: NotEstablished,
            batch_classification_historical_correctness: NotEstablished,
            physical_retirement: NotEstablished,
            journal_retirement_and_ownership: NotEstablished,
            recovery_convergence: NotEstablished,
            crossing_permission: NotEstablished,
            startup_safety: NotEstablished,
        }
    }
}

// ------------------------------------------------------------- the observation

/// The terminus that established a negative observation — only a
/// COMPLETE, uninterrupted, bounded walk over the relevant search
/// segment may produce one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotObservedTerminus {
    /// The walk reached the campaign founding without encountering
    /// the terminal or any checkpoint boundary.
    Founding { record: Hash },
    /// The walk reached the EXPECTED covering checkpoint itself —
    /// everything above it was inspected, and the terminal can only
    /// exist above its own covering MK.
    ExpectedCoveringCheckpoint { mk: Hash },
}

/// The head-relative Range-terminal installation observation. Every
/// variant — positive, negative, and unresolved — carries the
/// caller-supplied HEAD identity, so every outcome remains
/// attributable to the exact observation seed. No variant contains
/// an authorization token, deletion permit, skip permit,
/// canonical-head certificate, or startup-safe verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeInstallationObservation {
    /// The terminal and its full retained evidence segment were
    /// coherently observed on the ancestry reachable from the
    /// supplied head. HEAD-RELATIVE OBSERVATION ONLY — see the
    /// module's authority boundary.
    ObservedOnAncestry {
        /// The caller-supplied observation seed.
        observed_head: Hash,
        /// The C4 structural result over the observed terminal
        /// (boxed: the observation's other variants stay small and
        /// cheap to move).
        candidate: Box<StructuralRangeCandidate>,
        /// The covering MK actually reached and authenticated as a
        /// retained-evidence boundary.
        covering_mk: Hash,
        /// The observed V2 plan revision identity.
        plan_record: Hash,
        /// The observed V2 begin revision identity.
        begin_record: Hash,
        /// The observed Batch-marker revision identities, in the
        /// plan's deterministic batch order.
        batch_markers: Vec<Hash>,
        /// Every authority this observation explicitly does NOT
        /// establish.
        unresolved: UnresolvedObligations,
    },
    /// The terminal is definitively NOT on the inspected ancestry —
    /// established only by a complete, uninterrupted, bounded walk
    /// to the founding or to the EXPECTED covering checkpoint.
    NotObservedOnAncestry {
        /// The caller-supplied observation seed.
        observed_head: Hash,
        /// The terminus that completed the search.
        terminus: NotObservedTerminus,
    },
    /// A DIFFERENT checkpoint boundary was encountered before the
    /// requested observation could complete. The terminal may exist
    /// below it; absence is NOT claimed, and the boundary is never
    /// crossed (no skip tables, no traversal into the potentially
    /// reclaimed range).
    HiddenBehindCheckpointBoundary {
        /// The caller-supplied observation seed.
        observed_head: Hash,
        /// the foreign covering checkpoint's MK identity.
        boundary_mk: Hash,
    },
    /// A parent record is missing on the inspected ancestry —
    /// possibly a legitimately retired intervening range. Never
    /// resolved here, never an invented skip, never an absence.
    AncestryGapUnresolved {
        /// The caller-supplied observation seed.
        observed_head: Hash,
        /// The missing parent identity.
        at: Hash,
    },
}

// ------------------------------------------- the provider-coordinated observation (C5b)

/// The provider-coordinated not-established obligations — the eleven
/// C5a authorities PLUS the head-currency limitation. Every field is
/// a NAMED ABSENCE of authority, never a capability, never a
/// verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinatedUnresolvedObligations {
    /// The provider-coordinated canonical HEAD (established only
    /// within the coordination window).
    pub provider_canonical_head: NotEstablished,
    /// The begin's provider durability authority.
    pub begin_durability_authority: NotEstablished,
    /// The founding-derived seed's authenticity (below the MK).
    pub founding_derived_seed_authenticity: NotEstablished,
    /// The original range digest's historical correctness.
    pub original_range_digest_historical_correctness: NotEstablished,
    /// The original object closures below the MK.
    pub original_object_closures: NotEstablished,
    /// The Batch markers' historical classification correctness.
    pub batch_classification_historical_correctness: NotEstablished,
    /// The physical retirement of any identity.
    pub physical_retirement: NotEstablished,
    /// The journal retirement and ownership updates.
    pub journal_retirement_and_ownership: NotEstablished,
    /// The recovery convergence of any range.
    pub recovery_convergence: NotEstablished,
    /// Any checkpoint-crossing permission.
    pub crossing_permission: NotEstablished,
    /// Any startup-safety permission.
    pub startup_safety: NotEstablished,
    /// The HEAD's CURRENCY BEYOND THE COORDINATION SCOPE: after the
    /// scope closes, a later publication may supersede the head —
    /// the observation is an honest instant-in-time snapshot, never
    /// a currency certificate.
    pub head_currency_beyond_scope: NotEstablished,
}

impl CoordinatedUnresolvedObligations {
    /// The complete list — a coordinated observation carries every
    /// not-established authority explicitly.
    pub fn all() -> Self {
        CoordinatedUnresolvedObligations {
            provider_canonical_head: NotEstablished,
            begin_durability_authority: NotEstablished,
            founding_derived_seed_authenticity: NotEstablished,
            original_range_digest_historical_correctness: NotEstablished,
            original_object_closures: NotEstablished,
            batch_classification_historical_correctness: NotEstablished,
            physical_retirement: NotEstablished,
            journal_retirement_and_ownership: NotEstablished,
            recovery_convergence: NotEstablished,
            crossing_permission: NotEstablished,
            startup_safety: NotEstablished,
            head_currency_beyond_scope: NotEstablished,
        }
    }
}

/// The provider-coordinated Range-terminal observation (C5b): the
/// accepted C5a observation over a head that was read UNDER the
/// provider's own coordination scope, with the whole walk inside
/// that scope. **The promoted claim, exactly:** at a definite
/// instant inside a coordinated read scope, the coordinated head
/// was the provider's authoritative HEAD, and the `/3` Range
/// terminal with its retained evidence was observed on that head's
/// ancestry — NOTHING more. It does NOT establish retirement or
/// discharge of any obligation (physical, journal, ownership),
/// historical classification correctness, the founding-derived
/// facts below the MK, durability grades, or any deletion,
/// crossing, or startup permission — and the head's currency
/// beyond the coordination window is explicitly NOT established
/// (see [`CoordinatedUnresolvedObligations`]). No authorization
/// token, deletion permit, skip permit, canonical-head certificate,
/// or startup verdict exists anywhere in this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatedObservation {
    /// The head as read INSIDE the coordination scope — the walk's
    /// seed (every nested `observed_head` equals this identity).
    pub coordinated_head: Hash,
    /// The C5a observation, unchanged.
    pub observation: RangeInstallationObservation,
    /// Every authority this result does NOT establish.
    pub unresolved: CoordinatedUnresolvedObligations,
}

// ------------------------------------------------------- the expected key set

/// The precomputed deterministic keys and prefixes for the expected
/// MK — every classification decision is an exact byte comparison
/// against these.
struct ExpectedKeys {
    range_terminal: Vec<u8>,
    expected_begin: Vec<u8>,
    expected_plan: Vec<u8>,
    v2_batch_prefix: Vec<u8>,
    v2_ledger_prefix: Vec<u8>,
    v1_completion_prefix: Vec<u8>,
    v1_ledger_key: Vec<u8>,
}

impl ExpectedKeys {
    fn new(mk: &Hash) -> Self {
        let mk_hex = mk.hex();
        ExpectedKeys {
            range_terminal: crate::range_completion::reclamation_complete_v3_range_idempotency_key(
                mk,
            ),
            expected_begin: reclamation_begin_idempotency_key(mk),
            expected_plan: reclamation_plan_idempotency_key(mk),
            v2_batch_prefix: format!("{RECLAMATION_COMPLETE_V2_KEY_DOMAIN}:{mk_hex}:").into_bytes(),
            v2_ledger_prefix: format!("{RECLAMATION_LEDGER_V2_KEY_DOMAIN}:{mk_hex}:").into_bytes(),
            v1_completion_prefix: format!("{RECLAMATION_COMPLETE_KEY_DOMAIN}:{mk_hex}:")
                .into_bytes(),
            v1_ledger_key: reclamation_ledger_idempotency_key(mk),
        }
    }
}

/// Parses the batch index from a deterministic key's remainder
/// after the expected prefix (the exact decimal form).
fn parse_batch_suffix(key: &[u8], prefix: &[u8]) -> Result<u64, MediumError> {
    let remainder = &key[prefix.len()..];
    let text = std::str::from_utf8(remainder).map_err(|_| MediumError::Chain {
        rule: "range-observation/malformed-evidence-key",
    })?;
    text.parse::<u64>().map_err(|_| MediumError::Chain {
        rule: "range-observation/malformed-evidence-key",
    })
}

/// Whether a key in a known family carries an unsupported version
/// segment — the fail-closed unknown-variant rule.
fn unsupported_family_version(key: &[u8], family: &str, supported: &[&[u8]]) -> bool {
    let Some(rest) = key.strip_prefix(family.as_bytes()) else {
        return false;
    };
    let version_end = rest
        .iter()
        .position(|&byte| byte == b':')
        .unwrap_or(rest.len());
    let version = &rest[..version_end];
    !supported.contains(&version)
}

// ------------------------------------------------------------- the walk state

/// The temporal phase of the segment walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Above the terminal (or no terminal at all).
    PreTerminal,
    /// Below the terminal, above the begin.
    PostTerminalPreBegin,
    /// Below the begin, above the plan.
    PostBeginPrePlan,
    /// Below the plan, awaiting the covering MK.
    PostPlan,
}

/// The collected segment evidence (bounded: O(batch-count)
/// identities, no payload histories).
#[derive(Default)]
struct SegmentWalk {
    seen_identities: std::collections::BTreeSet<Hash>,
    seen_keys: std::collections::BTreeSet<Vec<u8>>,
    phase: Option<Phase>,
    expected_evidence_above: bool,
    terminal: Option<Hash>,
    batch_markers: std::collections::BTreeMap<u64, Hash>,
    begin_record: Option<Hash>,
    audits: std::collections::BTreeMap<u64, Hash>,
    plan_record: Option<Hash>,
}

impl SegmentWalk {
    fn new() -> Self {
        SegmentWalk {
            phase: Some(Phase::PreTerminal),
            ..SegmentWalk::default()
        }
    }

    fn phase(&self) -> Phase {
        self.phase.expect("the phase is always set")
    }
}

// ------------------------------------------------------------- the verifier

/// (V2X-CP3-B-P2A-C5a) The head-relative Range-terminal installation
/// OBSERVATION verifier: ONE bounded, reader-generic, read-only walk
/// from the caller-supplied HEAD down the parent chain, classifying
/// every consulted revision by its deterministic key, enforcing the
/// temporal publication order, and — when the expected covering MK
/// is reached — verifying the retained-evidence consistency layer
/// and rederiving both commitments from the OBSERVED segment
/// evidence. See the module header for the complete authority
/// boundary; nothing this function returns confers installation
/// authority beyond the head-relative observation itself.
pub fn verify_range_terminal_on_observed_ancestry<R: crate::reclaim::BeginReads>(
    reads: &mut R,
    observed_head: &Hash,
    expected: &RangeCompletionContext,
    limits: &RangeInstallationLimits,
) -> Result<RangeInstallationObservation, MediumError> {
    expected.validate().map_err(MediumError::Format)?;
    limits.validate()?;
    let keys = ExpectedKeys::new(&expected.mk);
    let mut walk = SegmentWalk::new();
    let mut cursor = *observed_head;
    let mut previous_epoch: Option<u64> = None;
    let mut consulted: u64 = 0;
    loop {
        if consulted >= limits.max_records {
            return Err(MediumError::Chain {
                rule: "range-observation/bound-exhausted",
            });
        }
        consulted += 1;
        // The record at the cursor: read, hash-verify, strictly
        // decode. A missing PARENT classifies as the unresolved
        // gap; a missing SUPPLIED HEAD is a caller-input refusal.
        let record_bytes = match reads.read_record_bytes(&cursor) {
            Ok(bytes) => bytes,
            Err(MediumError::MissingObject { .. }) if consulted > 1 => {
                return Ok(RangeInstallationObservation::AncestryGapUnresolved {
                    observed_head: *observed_head,
                    at: cursor,
                });
            }
            Err(other) => return Err(other),
        };
        if hash_object(&record_bytes) != cursor {
            return Err(MediumError::Chain {
                rule: "range-observation/record-hash",
            });
        }
        let record = RevisionRecord::decode(&record_bytes).map_err(MediumError::Format)?;
        if !walk.seen_identities.insert(cursor) {
            return Err(MediumError::Chain {
                rule: "range-observation/duplicate-identity",
            });
        }
        if let Some(child_epoch) = previous_epoch
            && record.epoch + 1 != child_epoch
        {
            return Err(MediumError::Chain {
                rule: "range-observation/epoch-linkage",
            });
        }
        // The founding terminus: a complete negative observation
        // only ABOVE the terminal; below it, the covering MK was
        // never authenticated.
        if matches!(record.kind, RecordKind::Founding { .. }) {
            return match walk.phase() {
                Phase::PreTerminal => Ok(RangeInstallationObservation::NotObservedOnAncestry {
                    observed_head: *observed_head,
                    terminus: NotObservedTerminus::Founding { record: cursor },
                }),
                _ => Err(MediumError::Chain {
                    rule: "range-observation/covering-mk-not-authenticated",
                }),
            };
        }
        // The keyed classification.
        if let Some(key) = record.idempotency_key.clone() {
            if key == keys.range_terminal {
                if !walk.seen_keys.insert(key) {
                    return Err(MediumError::Chain {
                        rule: "range-observation/duplicate-key",
                    });
                }
                if walk.phase() != Phase::PreTerminal {
                    return Err(MediumError::Chain {
                        rule: "range-observation/duplicate-key",
                    });
                }
                if walk.expected_evidence_above {
                    return Err(MediumError::Chain {
                        rule: "range-observation/evidence-above-terminal",
                    });
                }
                walk.terminal = Some(cursor);
                walk.phase = Some(Phase::PostTerminalPreBegin);
            } else if key.starts_with(keys.v2_batch_prefix.as_slice()) {
                if !walk.seen_keys.insert(key.clone()) {
                    return Err(MediumError::Chain {
                        rule: "range-observation/duplicate-key",
                    });
                }
                let batch = parse_batch_suffix(&key, &keys.v2_batch_prefix)?;
                match walk.phase() {
                    Phase::PreTerminal => walk.expected_evidence_above = true,
                    Phase::PostTerminalPreBegin => {
                        if walk.batch_markers.insert(batch, cursor).is_some() {
                            return Err(MediumError::Chain {
                                rule: "range-observation/duplicate-batch-marker",
                            });
                        }
                    }
                    _ => {
                        return Err(MediumError::Chain {
                            rule: "range-observation/marker-below-begin",
                        });
                    }
                }
            } else if key == keys.expected_begin {
                if !walk.seen_keys.insert(key) {
                    return Err(MediumError::Chain {
                        rule: "range-observation/duplicate-key",
                    });
                }
                match walk.phase() {
                    Phase::PreTerminal => walk.expected_evidence_above = true,
                    Phase::PostTerminalPreBegin => {
                        walk.begin_record = Some(cursor);
                        walk.phase = Some(Phase::PostBeginPrePlan);
                    }
                    _ => {
                        return Err(MediumError::Chain {
                            rule: "range-observation/begin-misordered",
                        });
                    }
                }
            } else if key == keys.expected_plan {
                if !walk.seen_keys.insert(key) {
                    return Err(MediumError::Chain {
                        rule: "range-observation/duplicate-key",
                    });
                }
                match walk.phase() {
                    Phase::PreTerminal => walk.expected_evidence_above = true,
                    Phase::PostTerminalPreBegin => {
                        return Err(MediumError::Chain {
                            rule: "range-observation/plan-above-begin",
                        });
                    }
                    Phase::PostBeginPrePlan => {
                        walk.plan_record = Some(cursor);
                        walk.phase = Some(Phase::PostPlan);
                    }
                    Phase::PostPlan => {
                        return Err(MediumError::Chain {
                            rule: "range-observation/duplicate-key",
                        });
                    }
                }
            } else if key.starts_with(keys.v2_ledger_prefix.as_slice()) {
                if !walk.seen_keys.insert(key.clone()) {
                    return Err(MediumError::Chain {
                        rule: "range-observation/duplicate-key",
                    });
                }
                let batch = parse_batch_suffix(&key, &keys.v2_ledger_prefix)?;
                match walk.phase() {
                    Phase::PreTerminal => walk.expected_evidence_above = true,
                    Phase::PostTerminalPreBegin => {
                        return Err(MediumError::Chain {
                            rule: "range-observation/audit-above-begin",
                        });
                    }
                    Phase::PostBeginPrePlan => {
                        if walk.audits.insert(batch, cursor).is_some() {
                            return Err(MediumError::Chain {
                                rule: "range-observation/duplicate-audit",
                            });
                        }
                    }
                    Phase::PostPlan => {
                        return Err(MediumError::Chain {
                            rule: "range-observation/audit-below-plan",
                        });
                    }
                }
            } else if key.starts_with(CHECKPOINT_MK_KEY_PREFIX.as_bytes()) {
                if unsupported_family_version(&key, CHECKPOINT_MK_KEY_PREFIX, &[b"1"]) {
                    return Err(MediumError::Chain {
                        rule: "range-observation/unsupported-family-variant",
                    });
                }
                if !walk.seen_keys.insert(key) {
                    return Err(MediumError::Chain {
                        rule: "range-observation/duplicate-key",
                    });
                }
                // A genuine checkpoint boundary: the single-ref
                // shape, the hash-verified metadata object, the
                // strict decode, the exact derived key, the index
                // shape, and the SK-parent binding.
                if record.content_refs.len() != 1 {
                    return Err(MediumError::Chain {
                        rule: "range-observation/mk-shape",
                    });
                }
                let metadata_object = record.content_refs[0];
                let meta_bytes = reads.read_object_bytes(&metadata_object)?;
                if hash_object(&meta_bytes) != metadata_object {
                    return Err(MediumError::Chain {
                        rule: "range-observation/mk-object-hash",
                    });
                }
                let metadata =
                    CheckpointMetadata::decode(&meta_bytes).map_err(MediumError::Format)?;
                if record.idempotency_key.as_deref()
                    != Some(checkpoint_mk_idempotency_key(&metadata.sk).as_slice())
                {
                    return Err(MediumError::Chain {
                        rule: "range-observation/mk-key-mismatch",
                    });
                }
                crate::ancestry_witness::validate_index_shape(&metadata.index)
                    .map_err(MediumError::Format)?;
                if record.parent != Some(metadata.sk) {
                    return Err(MediumError::Chain {
                        rule: "range-observation/mk-sk-binding",
                    });
                }
                return match walk.phase() {
                    Phase::PreTerminal => {
                        if cursor == expected.mk {
                            Ok(RangeInstallationObservation::NotObservedOnAncestry {
                                observed_head: *observed_head,
                                terminus: NotObservedTerminus::ExpectedCoveringCheckpoint {
                                    mk: cursor,
                                },
                            })
                        } else {
                            Ok(
                                RangeInstallationObservation::HiddenBehindCheckpointBoundary {
                                    observed_head: *observed_head,
                                    boundary_mk: cursor,
                                },
                            )
                        }
                    }
                    Phase::PostPlan if cursor == expected.mk => complete_observation(
                        reads,
                        observed_head,
                        expected,
                        &walk,
                        cursor,
                        metadata,
                    ),
                    Phase::PostPlan => Err(MediumError::Chain {
                        rule: "range-observation/foreign-boundary-in-segment",
                    }),
                    _ => Err(MediumError::Chain {
                        rule: "range-observation/boundary-before-evidence",
                    }),
                };
            } else if key.starts_with(keys.v1_completion_prefix.as_slice())
                || key == keys.v1_ledger_key
            {
                // The V1 evidence family for the EXPECTED MK — the
                // W2 bidirectional exclusion: a mixed V1/V2 state
                // for one MK refuses.
                return Err(MediumError::Chain {
                    rule: "range-observation/v1-mixed-evidence",
                });
            } else {
                // A key in a known family with an unsupported
                // version refuses closed; known versions for
                // foreign MKs are ordinary observed content.
                if key.starts_with(COMPLETION_KEY_FAMILY.as_bytes())
                    && unsupported_family_version(&key, COMPLETION_KEY_FAMILY, &[b"1", b"2", b"3"])
                {
                    return Err(MediumError::Chain {
                        rule: "range-observation/unsupported-family-variant",
                    });
                }
                if key.starts_with(BEGIN_KEY_FAMILY.as_bytes())
                    && unsupported_family_version(&key, BEGIN_KEY_FAMILY, &[b"1"])
                {
                    return Err(MediumError::Chain {
                        rule: "range-observation/unsupported-family-variant",
                    });
                }
                if key.starts_with(PLAN_KEY_FAMILY.as_bytes())
                    && unsupported_family_version(&key, PLAN_KEY_FAMILY, &[b"1"])
                {
                    return Err(MediumError::Chain {
                        rule: "range-observation/unsupported-family-variant",
                    });
                }
                if key.starts_with(LEDGER_KEY_FAMILY.as_bytes())
                    && unsupported_family_version(&key, LEDGER_KEY_FAMILY, &[b"1", b"2"])
                {
                    return Err(MediumError::Chain {
                        rule: "range-observation/unsupported-family-variant",
                    });
                }
                // Otherwise: foreign-MK evidence or unrelated keyed
                // content — ordinary, observed, never interpreted.
            }
        }
        // Move down the parent chain.
        let Some(parent) = record.parent else {
            return Err(MediumError::Chain {
                rule: "range-observation/malformed-regular",
            });
        };
        previous_epoch = Some(record.epoch);
        cursor = parent;
    }
}

// --------------------------------------------------- the completion (R2 + R5)

/// Reads and strictly decodes the single-object payload of an
/// already-observed evidence revision (the record re-read,
/// hash-verified, and shape-checked — the self-contained reader
/// discipline).
fn decode_evidence_payload<R: crate::reclaim::BeginReads, T>(
    reads: &mut R,
    record_id: &Hash,
    decode: fn(&[u8]) -> Result<T, engine_core::SaveError>,
) -> Result<(Hash, T), MediumError> {
    let record_bytes = reads.read_record_bytes(record_id)?;
    if hash_object(&record_bytes) != *record_id {
        return Err(MediumError::Chain {
            rule: "range-observation/record-hash",
        });
    }
    let record = RevisionRecord::decode(&record_bytes).map_err(MediumError::Format)?;
    if record.content_refs.len() != 1 {
        return Err(MediumError::Chain {
            rule: "range-observation/evidence-shape",
        });
    }
    let object_hash = record.content_refs[0];
    let object_bytes = reads.read_object_bytes(&object_hash)?;
    if hash_object(&object_bytes) != object_hash {
        return Err(MediumError::Chain {
            rule: "range-observation/object-hash",
        });
    }
    let payload = decode(&object_bytes).map_err(MediumError::Format)?;
    Ok((object_hash, payload))
}

/// The retained-evidence consistency layer + the commitment
/// rederivation, run after the EXPECTED covering MK is reached.
fn complete_observation<R: crate::reclaim::BeginReads>(
    reads: &mut R,
    observed_head: &Hash,
    expected: &RangeCompletionContext,
    walk: &SegmentWalk,
    mk: Hash,
    metadata: CheckpointMetadata,
) -> Result<RangeInstallationObservation, MediumError> {
    let plan_record = walk
        .plan_record
        .expect("the plan was observed in Phase::PostPlan");
    let begin_record = walk.begin_record.expect("the begin was observed");
    let terminal = walk.terminal.expect("the terminal was found");
    // The caller's expected context must agree with the observed
    // segment — a caller-supplied context is never independently
    // authoritative, and a disagreement refuses.
    if plan_record != expected.plan || begin_record != expected.begin {
        return Err(MediumError::Chain {
            rule: "range-observation/context-disagreement",
        });
    }
    // The observed V2 plan: its deterministic key was matched during
    // the walk; here its payload bindings.
    let (_, plan) = decode_evidence_payload(reads, &plan_record, ReclaimPlan::decode)?;
    if plan.mk != mk
        || plan.sk != metadata.sk
        || plan.range_digest != metadata.range_digest
        || plan.seed != metadata.seed
    {
        return Err(MediumError::Chain {
            rule: "range-observation/plan-bindings",
        });
    }
    if plan.ledger_byte_limit > MAX_RECLAMATION_LEDGER_BYTES {
        return Err(MediumError::Chain {
            rule: "range-observation/plan-ledger-ceiling",
        });
    }
    // The plan-index partition: the flattened plan membership,
    // position-by-position, against the MK index's epoch-descending
    // retirement membership.
    let ordered = crate::ancestry_witness::canonical_member_order(&metadata.index);
    let mut flattened = plan.batches.iter().flat_map(|batch| batch.records.iter());
    for entry in &ordered {
        match flattened.next() {
            Some(record) if *record == entry.hash => {}
            _ => {
                return Err(MediumError::Chain {
                    rule: "range-observation/plan-index-partition",
                });
            }
        }
    }
    if flattened.next().is_some() {
        return Err(MediumError::Chain {
            rule: "range-observation/plan-index-partition",
        });
    }
    // The audit set must equal the plan's batch set exactly (no
    // missing, no foreign).
    if walk.audits.len() != plan.batches.len() {
        return Err(MediumError::Chain {
            rule: "range-observation/audit-set-mismatch",
        });
    }
    // The index lookup for the reclaimed_keys recomputation.
    let indexed: std::collections::BTreeMap<Hash, bool> = metadata
        .index
        .iter()
        .map(|entry| (entry.hash, entry.key.is_some()))
        .collect();
    // Every plan-defined audit, one payload at a time: the record
    // and object hash-verified, the shape checked, the admitted
    // byte limit enforced BEFORE decoding, the payload strictly
    // decoded and its bindings, ordered membership, and recomputed
    // reclaimed_keys verified.
    for batch in &plan.batches {
        let PlanBatch { index, records } = batch;
        let Some(audit_record) = walk.audits.get(index) else {
            return Err(MediumError::Chain {
                rule: "range-observation/audit-missing",
            });
        };
        let record_bytes = reads.read_record_bytes(audit_record)?;
        if hash_object(&record_bytes) != *audit_record {
            return Err(MediumError::Chain {
                rule: "range-observation/record-hash",
            });
        }
        let audit_record_decoded =
            RevisionRecord::decode(&record_bytes).map_err(MediumError::Format)?;
        if audit_record_decoded.content_refs.len() != 1 {
            return Err(MediumError::Chain {
                rule: "range-observation/evidence-shape",
            });
        }
        let audit_object = audit_record_decoded.content_refs[0];
        let audit_bytes = reads.read_object_bytes(&audit_object)?;
        if hash_object(&audit_bytes) != audit_object {
            return Err(MediumError::Chain {
                rule: "range-observation/object-hash",
            });
        }
        // The admitted byte limit, enforced before decoding.
        if audit_bytes.len() as u64 > plan.ledger_byte_limit {
            return Err(MediumError::Chain {
                rule: "range-observation/audit-byte-limit",
            });
        }
        let ledger = ReclamationLedgerV2::decode(&audit_bytes).map_err(MediumError::Format)?;
        if ledger.mk != mk
            || ledger.sk != metadata.sk
            || ledger.batch != *index
            || ledger.range_digest != plan.range_digest
            || ledger.seed != plan.seed
        {
            return Err(MediumError::Chain {
                rule: "range-observation/audit-bindings",
            });
        }
        // The exact ordered unit-record membership.
        if ledger.units.len() != records.len() {
            return Err(MediumError::Chain {
                rule: "range-observation/audit-membership",
            });
        }
        for (unit, record) in ledger.units.iter().zip(records.iter()) {
            if unit.record != *record {
                return Err(MediumError::Chain {
                    rule: "range-observation/audit-membership",
                });
            }
        }
        // The reclaimed_keys recomputed from the retained index's
        // keyed-member information.
        let keyed = records
            .iter()
            .filter(|record| indexed.get(*record).copied().unwrap_or(false))
            .count() as u64;
        if ledger.reclaimed_keys != keyed {
            return Err(MediumError::Chain {
                rule: "range-observation/audit-reclaimed-keys",
            });
        }
    }
    // The observed begin's payload bindings to the observed plan.
    let (_, begin) = decode_evidence_payload(reads, &begin_record, ReclamationBegin::decode)?;
    if begin.mk != mk
        || begin.source != plan_record
        || begin.range_digest != plan.range_digest
        || begin.seed != plan.seed
    {
        return Err(MediumError::Chain {
            rule: "range-observation/begin-bindings",
        });
    }
    // The terminal through the C4 reader, over the
    // observed-assembled context.
    let context = RangeCompletionContext {
        mk,
        plan: plan_record,
        begin: begin_record,
    };
    let candidate = read_structural_range_candidate(reads, &terminal, &context)?;
    // The Batch markers through the C2 reader, over the
    // observed-assembled contexts, in the plan's order.
    if walk.batch_markers.len() != plan.batches.len() {
        return Err(MediumError::Chain {
            rule: "range-observation/marker-set-mismatch",
        });
    }
    let mut observations = Vec::with_capacity(plan.batches.len());
    let mut batch_markers = Vec::with_capacity(plan.batches.len());
    for batch in &plan.batches {
        let Some(marker_record) = walk.batch_markers.get(&batch.index) else {
            return Err(MediumError::Chain {
                rule: "range-observation/marker-missing",
            });
        };
        let audit_record = walk.audits[&batch.index];
        let batch_context = BatchCompletionContext {
            mk,
            plan: plan_record,
            batch_index: batch.index,
            audit: audit_record,
            begin: begin_record,
        };
        let batch_candidate =
            read_structural_batch_candidate(reads, marker_record, &batch_context)?;
        observations.push(RangeCoverageObservation {
            batch_index: batch.index,
            marker_record: *marker_record,
            counters: batch_candidate.marker.counters,
        });
        batch_markers.push(*marker_record);
    }
    // The commitments, rederived from the OBSERVED segment evidence.
    candidate
        .marker
        .verify_coverage(&observations)
        .map_err(MediumError::Format)?;
    candidate
        .marker
        .verify_journal_obligations(&plan.journal_files)
        .map_err(MediumError::Format)?;
    Ok(RangeInstallationObservation::ObservedOnAncestry {
        observed_head: *observed_head,
        candidate: Box::new(candidate),
        covering_mk: mk,
        plan_record,
        begin_record,
        batch_markers,
        unresolved: UnresolvedObligations::all(),
    })
}

/// (V2X-CP3-B-P2A-C5b) The shared coordinated core: the accepted C5a
/// verifier run over a head that the CALLER read under the
/// provider's own coordination scope, with this whole walk inside
/// that same scope (the provider's locked-reads or
/// transaction-scoped adapter supplies `reads`). The invariant is
/// by construction: the coordinated head IS the walk's seed, so
/// every result variant's `observed_head` equals the
/// `coordinated_head`. Read-only; no publication, no mutation, no
/// new authority beyond the head-relative observation's own
/// contract (see [`CoordinatedObservation`]).
pub(crate) fn observe_coordinated<R: crate::reclaim::BeginReads>(
    reads: &mut R,
    coordinated_head: &Hash,
    expected: &RangeCompletionContext,
    limits: &RangeInstallationLimits,
) -> Result<CoordinatedObservation, MediumError> {
    let observation =
        verify_range_terminal_on_observed_ancestry(reads, coordinated_head, expected, limits)?;
    Ok(CoordinatedObservation {
        coordinated_head: *coordinated_head,
        observation,
        unresolved: CoordinatedUnresolvedObligations::all(),
    })
}

// ===================================================== the C5d composite
//
// (V2X-CP3-B-P2A-C5d) The discovery-derived coordinated
// Range-terminal observation — the startup prerequisite. C5a
// verifies an EXPECTED context; C5b coordinates the head under
// the provider's scope; C5d removes the last caller-supplied
// input: the context itself, derived from the retained evidence
// the recovery discovery observes on the inspected segment.
//
// **The two-provenance discipline.** Stage one is the accepted
// recovery discovery ([`crate::recovery_discovery`]) run
// UNCOORDINATED from a caller-supplied head — the discovery's own
// documented seed semantics ("the head is an observation seed,
// never an authority"). The derivation consumes hash-verified
// structural identities only: the covering checkpoint's MK
// record, the newest plan revision, and the newest begin
// revision above it. Stage three is the accepted C5b coordinated
// observation over the DERIVED context — the provider-scoped,
// authoritative installation answer FOR THAT CONTEXT. The
// composite never claims the discovered range is THE active
// range: if the head moved between the stages, the observation
// still answers truthfully for the derived context on the
// coordinated head's ancestry (the same snapshot semantics as
// [`CoordinatedUnresolvedObligations::head_currency_beyond_scope`]).
//
// **What this composite may NOT establish — by construction:** a
// canonical-head claim beyond the coordination window; recovery
// or retirement verdicts (the classifier's states own those);
// V1-evidence disposition (the existing V1 verifiers own it);
// any capability, permission, cursor, or destructive authority.
// The not-derivable ladder below is an honest classification of
// what the discovery observed, never an invented obligation.

/// The typed two-stage refusals of the composite — the discovery
/// stage's and the observation stage's own errors, propagated
/// separately and never collapsed.
#[derive(Debug)]
pub enum DiscoveredObservationError {
    /// The discovery stage's typed refusal (a missing head record,
    /// a missing parent, a corrupt record, a cycle, or an
    /// exhausted traversal bound — see
    /// [`crate::recovery_discovery::DiscoveryError`]).
    Discovery(crate::recovery_discovery::DiscoveryError),
    /// The coordinated observation stage's typed refusal (the
    /// C5b surface's own error, verbatim).
    Observation(MediumError),
}

/// What the derivation stage established about the candidate V2
/// range context — the honest ladder over the discovery's
/// structural observations. Every variant names what was
/// OBSERVED; none is a verdict about execution, installation
/// canonicity, durability, or retirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveredRangeContext {
    /// The full V2 context derived: the covering MK record, the
    /// newest plan revision, and the newest begin whose source
    /// binds that plan (the V2 shape — the V1 begin binds an
    /// audit). All identities are the discovery's hash-verified
    /// structural observations; the coordinated observation stage
    /// owns every binding and temporal check on them.
    V2Context {
        /// The derived context (the C5b observation's input).
        context: RangeCompletionContext,
        /// The covering checkpoint's MK record identity (the
        /// derivation's anchor).
        covering_mk: Hash,
        /// How many records the discovery consulted for this
        /// derivation (the seed-relative walk's own accounting).
        records_consulted: u64,
    },
    /// The inspected ancestry reached the founding record with no
    /// covering checkpoint — no range context can be anchored
    /// head-relatively.
    NoCoveringCheckpoint {
        /// The founding record the discovery reached.
        founding: Hash,
    },
    /// A covering checkpoint was identified but no V2 plan
    /// revision was observed above it — no V2 reclamation was
    /// ever begun on the inspected segment.
    NoPlanAboveCovering {
        /// The covering checkpoint's MK record identity.
        covering_mk: Hash,
    },
    /// A plan was observed but no begin revision was observed
    /// above the covering checkpoint — the reclamation never
    /// began on the inspected segment (or its begin lies below
    /// the boundary, which the discovery never reads).
    NoBeginAbovePlan {
        /// The covering checkpoint's MK record identity.
        covering_mk: Hash,
        /// The newest observed plan revision's identity.
        plan: Hash,
    },
    /// A begin was observed whose source binds an audit revision,
    /// not the plan (the V1 shape) — the V2 Range-terminal
    /// observation does not apply; the existing V1 verifiers own
    /// this evidence.
    V1BeginObserved {
        /// The covering checkpoint's MK record identity.
        covering_mk: Hash,
        /// The newest observed plan revision's identity.
        plan: Hash,
        /// The observed begin revision's identity.
        begin: Hash,
    },
}

/// The composite result: the derivation ladder plus, when a V2
/// context was derived, the C5b coordinated observation for that
/// context. The destructuring names everything held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRangeObservation {
    /// What the derivation stage established.
    pub derivation: DiscoveredRangeContext,
    /// The coordinated observation — present only for the derived
    /// V2 context (the not-derivable shapes carry no observation;
    /// they are honest classifications, never errors).
    pub observation: Option<CoordinatedObservation>,
}

/// (V2X-CP3-B-P2A-C5d) The discovery-derived coordinated
/// Range-terminal observation. Read-only over the supplied
/// medium; no publication, no mutation, no new authority beyond
/// the two composed stages' own contracts.
///
/// Stage one runs the accepted recovery discovery from the
/// caller-supplied `head` (the discovery's seed semantics —
/// uncoordinated, head-relative). Stage two derives the V2 range
/// context from the discovered evidence (or classifies the
/// not-derivable shape honestly). Stage three runs the accepted
/// C5b coordinated observation for the derived context under the
/// provider's own coordination scope.
pub fn observe_discovered_range_terminal_under_coordination(
    medium: &mut dyn crate::medium::CanonicalMedium,
    head: &Hash,
    discovery_limits: &crate::recovery_discovery::DiscoveryLimits,
    installation_limits: &RangeInstallationLimits,
) -> Result<DiscoveredRangeObservation, DiscoveredObservationError> {
    // Stage one — the discovery (the seed-relative structural
    // walk; the generic-medium adapter supplies the hash-verifying
    // reads without any provider coordination, per the discovery's
    // own documented contract).
    let mut reads = crate::reclaim::MediumReads::new(medium);
    let discovery =
        crate::recovery_discovery::discover_recovery_evidence(&mut reads, head, discovery_limits)
            .map_err(DiscoveredObservationError::Discovery)?;
    // Stage two — the context derivation.
    let derivation = derive_range_completion_context(&discovery);
    // Stage three — the coordinated observation for the derived
    // context (the C5b surface opens the provider's own scope).
    let observation = match &derivation {
        DiscoveredRangeContext::V2Context { context, .. } => Some(
            medium
                .observe_range_terminal_under_coordination(context, installation_limits)
                .map_err(DiscoveredObservationError::Observation)?,
        ),
        _ => None,
    };
    Ok(DiscoveredRangeObservation {
        derivation,
        observation,
    })
}

/// The pure derivation: the covering MK, the newest plan, and the
/// newest plan-bound (V2) begin from the discovery's structural
/// observations — or the honest not-derivable classification. The
/// temporal order and every binding on the derived identities are
/// the observation stage's own checks (fail-closed); this
/// derivation performs no verification of its own beyond the V2
/// source-binding shape that selects which verifier owns the
/// evidence.
pub(crate) fn derive_range_completion_context(
    discovery: &crate::recovery_discovery::RecoveryEvidenceDiscovery,
) -> DiscoveredRangeContext {
    let covering_mk = match &discovery.covering {
        crate::recovery_discovery::CoveringObservation::Identified(checkpoint) => checkpoint.record,
        crate::recovery_discovery::CoveringObservation::NoneOnAncestry { founding } => {
            return DiscoveredRangeContext::NoCoveringCheckpoint {
                founding: *founding,
            };
        }
    };
    // The discovery order is newest → oldest: the FIRST plan and
    // the FIRST begin are the newest of each.
    let Some(plan) = discovery.records.iter().find(|record| {
        matches!(
            record.family,
            crate::recovery_discovery::DiscoveredFamily::ReclamationPlan
        )
    }) else {
        return DiscoveredRangeContext::NoPlanAboveCovering { covering_mk };
    };
    let Some(begin) = discovery.records.iter().find(|record| {
        matches!(
            record.family,
            crate::recovery_discovery::DiscoveredFamily::ReclamationBegin
        )
    }) else {
        return DiscoveredRangeContext::NoBeginAbovePlan {
            covering_mk,
            plan: plan.identity,
        };
    };
    // The V2 shape: the begin's source binds the plan (the V1
    // begin binds an audit revision).
    let v2_shape = matches!(
        &begin.payload,
        crate::recovery_discovery::DiscoveredPayload::Begin(begin_payload)
            if begin_payload.source == plan.identity
    );
    if !v2_shape {
        return DiscoveredRangeContext::V1BeginObserved {
            covering_mk,
            plan: plan.identity,
            begin: begin.identity,
        };
    }
    DiscoveredRangeContext::V2Context {
        context: RangeCompletionContext {
            mk: covering_mk,
            plan: plan.identity,
            begin: begin.identity,
        },
        covering_mk,
        records_consulted: discovery.records_consulted,
    }
}

// ============================================================== the C5a tests
//
// The owner's required adversarial matrix — pure synthetic-reader
// tests over GENUINE canonical record/object codecs (the real
// metadata/plan/audit/begin/marker/terminal constructions, real
// deterministic keys, real parent/epoch linkage). No filesystem,
// no providers, no publication, no destructive mutation.

#[cfg(test)]
mod c5a_tests {
    use super::*;
    use crate::ancestry_accounting::StorageRole;
    use crate::batch_completion::{
        BatchCompletionMarkerV2, ClassificationCounters, ClassificationEntry, EntryClassification,
        commit_classification, reclamation_complete_v2_idempotency_key,
    };
    use crate::checkpoint::ReclaimedIndexEntry;
    use crate::range_completion::{
        RangeCompletionMarkerV3, commit_journal_obligations, commit_range_coverage,
        reclamation_complete_v3_range_idempotency_key,
    };
    use crate::reclaim::{PlanBatch, ReclaimUnit, reclamation_ledger_v2_idempotency_key};
    use crate::record::AuthorityMode;

    pub(super) struct SyntheticReads {
        records: std::collections::BTreeMap<Hash, Vec<u8>>,
        objects: std::collections::BTreeMap<Hash, Vec<u8>>,
    }
    impl crate::reclaim::BeginReads for SyntheticReads {
        fn read_record_bytes(&mut self, id: &Hash) -> Result<Vec<u8>, MediumError> {
            self.records
                .get(id)
                .cloned()
                .ok_or(MediumError::MissingObject { hash: *id })
        }
        fn read_object_bytes(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
            self.objects
                .get(hash)
                .cloned()
                .ok_or(MediumError::MissingObject { hash: *hash })
        }
    }

    pub(super) fn plain_object(payload: &[u8]) -> Vec<u8> {
        let mut enc = engine_core::Encoder::new();
        enc.write_str("sektor7/save/1");
        enc.write_bytes(payload);
        enc.into_bytes()
    }

    /// A canonical-chain builder: every append carries the strict
    /// parent and epoch linkage, and the deterministic key the
    /// caller names. `seeded` starts above a virtual parent (the
    /// walk never reads below the covering MK); `founded` starts at
    /// a genuine founding record.
    #[derive(Clone)]
    pub(super) struct ChainBuilder {
        records: std::collections::BTreeMap<Hash, Vec<u8>>,
        objects: std::collections::BTreeMap<Hash, Vec<u8>>,
        pub(super) head: Hash,
        head_epoch: u64,
        tick: u64,
    }

    impl ChainBuilder {
        pub(super) fn seeded(seed: Hash, seed_epoch: u64) -> Self {
            ChainBuilder {
                records: std::collections::BTreeMap::new(),
                objects: std::collections::BTreeMap::new(),
                head: seed,
                head_epoch: seed_epoch,
                tick: 0,
            }
        }

        pub(super) fn founded(campaign: &str) -> Self {
            let mut builder = ChainBuilder::seeded(Hash::ZERO, 0);
            let object_bytes = plain_object(b"c5a-founding");
            let object_hash = hash_object(&object_bytes);
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0)
                .with_content_refs(vec![object_hash]);
            let founding_id = founding.id();
            builder
                .records
                .insert(founding_id, founding.canonical_bytes());
            builder.objects.insert(object_hash, object_bytes);
            builder.head = founding_id;
            builder.head_epoch = 0;
            builder
        }

        pub(super) fn append(&mut self, key: Option<Vec<u8>>, object_bytes: Vec<u8>) -> Hash {
            let object_hash = hash_object(&object_bytes);
            self.tick += 1;
            let record = RevisionRecord::regular(self.head, self.head_epoch, self.tick)
                .with_content_refs(vec![object_hash]);
            let record = match key {
                Some(key) => record.with_idempotency_key(key),
                None => record,
            };
            let id = record.id();
            self.records.insert(id, record.canonical_bytes());
            self.objects.insert(object_hash, object_bytes);
            self.head = id;
            self.head_epoch += 1;
            id
        }

        pub(super) fn reader(&self) -> SyntheticReads {
            SyntheticReads {
                records: self.records.clone(),
                objects: self.objects.clone(),
            }
        }
    }

    /// The retained-evidence facts of one genuine retirement
    /// campaign (the members, the digests, the plan, the audits).
    pub(super) struct Facts {
        pub(super) mk: Hash,
        pub(super) plan_record: Hash,
        pub(super) audit_records: Vec<Hash>,
        pub(super) range_digest: [u8; 32],
        pub(super) seed: [u8; 32],
        plan: ReclaimPlan,
        sk: Hash,
        m1: Hash,
        m2: Hash,
        m3: Hash,
        m4: Hash,
    }

    impl Facts {
        pub(super) fn expected_context(&self, begin_record: Hash) -> RangeCompletionContext {
            RangeCompletionContext {
                mk: self.mk,
                plan: self.plan_record,
                begin: begin_record,
            }
        }
    }

    pub(super) fn the_members() -> [Hash; 5] {
        [
            hash_object(b"c5a-sk"),
            hash_object(b"c5a-m1"),
            hash_object(b"c5a-m2"),
            hash_object(b"c5a-m3"),
            hash_object(b"c5a-m4"),
        ]
    }

    pub(super) fn the_digests() -> ([u8; 32], [u8; 32]) {
        ([7u8; 32], [9u8; 32])
    }

    /// The genuine index: SK plus four members, epoch-descending,
    /// with two carrying idempotency keys (the reclaimed_keys
    /// recomputation's input).
    pub(super) fn the_index(members: &[Hash; 5]) -> Vec<ReclaimedIndexEntry> {
        vec![
            ReclaimedIndexEntry::new(None, members[0], 60),
            ReclaimedIndexEntry::new(Some(b"key-m1".to_vec()), members[1], 50),
            ReclaimedIndexEntry::new(None, members[2], 40),
            ReclaimedIndexEntry::new(Some(b"key-m3".to_vec()), members[3], 30),
            ReclaimedIndexEntry::new(None, members[4], 20),
        ]
    }

    /// The campaign prefix: the covering MK (over a virtual SK
    /// seed), the plan, and both audits — everything below the
    /// begin. The batches and the audits' unit lists and
    /// reclaimed_keys are parameters so the consistency adversaries
    /// can mutate them; the ledger payloads are constructed with
    /// the REAL facts (the MK identity and digests are known only
    /// after the MK append).
    pub(super) fn campaign_prefix(
        batches: Vec<PlanBatch>,
        audit0_units: Vec<ReclaimUnit>,
        audit0_keys: u64,
        audit1_units: Vec<ReclaimUnit>,
        audit1_keys: u64,
    ) -> (ChainBuilder, Facts) {
        let members = the_members();
        let (range_digest, seed) = the_digests();
        let mut builder = ChainBuilder::seeded(members[0], 60);
        let metadata = CheckpointMetadata::new(
            members[0],
            hash_object(b"c5a-sk-parent"),
            vec![],
            the_index(&members),
            range_digest,
            seed,
        );
        let mk = builder.append(
            Some(checkpoint_mk_idempotency_key(&members[0])),
            metadata.canonical_bytes(),
        );
        let plan = ReclaimPlan {
            mk,
            sk: members[0],
            range_digest,
            seed,
            ledger_byte_limit: 2048,
            batches,
            journal_files: vec!["journal.t1.0".to_owned(), "journal.t1.2".to_owned()],
        };
        let plan_record = builder.append(
            Some(reclamation_plan_idempotency_key(&mk)),
            plan.canonical_bytes(),
        );
        let audit0 = ReclamationLedgerV2 {
            mk,
            sk: members[0],
            batch: 0,
            range_digest,
            seed,
            records: audit0_units.len() as u64,
            reclaimed_keys: audit0_keys,
            units: audit0_units,
        };
        let audit0_record = builder.append(
            Some(reclamation_ledger_v2_idempotency_key(&mk, 0)),
            audit0.canonical_bytes(),
        );
        let audit1 = ReclamationLedgerV2 {
            mk,
            sk: members[0],
            batch: 1,
            range_digest,
            seed,
            records: audit1_units.len() as u64,
            reclaimed_keys: audit1_keys,
            units: audit1_units,
        };
        let audit1_record = builder.append(
            Some(reclamation_ledger_v2_idempotency_key(&mk, 1)),
            audit1.canonical_bytes(),
        );
        (
            builder,
            Facts {
                mk,
                plan,
                plan_record,
                audit_records: vec![audit0_record, audit1_record],
                sk: members[0],
                m1: members[1],
                m2: members[2],
                m3: members[3],
                m4: members[4],
                range_digest,
                seed,
            },
        )
    }

    /// The genuine audit unit lists (the records in the plan's
    /// batch order; the reclaimed_keys are 1 per batch — m1 and m3
    /// carry the index keys).
    pub(super) fn genuine_units(members: &[Hash; 5]) -> (Vec<ReclaimUnit>, Vec<ReclaimUnit>) {
        (
            vec![
                ReclaimUnit {
                    record: members[0],
                    objects: vec![],
                },
                ReclaimUnit {
                    record: members[1],
                    objects: vec![hash_object(b"c5a-o1")],
                },
            ],
            vec![
                ReclaimUnit {
                    record: members[2],
                    objects: vec![],
                },
                ReclaimUnit {
                    record: members[3],
                    objects: vec![],
                },
                ReclaimUnit {
                    record: members[4],
                    objects: vec![],
                },
            ],
        )
    }

    pub(super) fn append_begin(builder: &mut ChainBuilder, facts: &Facts) -> Hash {
        let begin = ReclamationBegin {
            mk: facts.mk,
            source: facts.plan_record,
            range_digest: facts.range_digest,
            seed: facts.seed,
        };
        builder.append(
            Some(reclamation_begin_idempotency_key(&facts.mk)),
            begin.canonical_bytes(),
        )
    }

    fn marker_entries(facts: &Facts, batch: usize) -> Vec<ClassificationEntry> {
        match batch {
            0 => vec![
                ClassificationEntry {
                    identity: facts.sk,
                    role: StorageRole::Record,
                    classification: EntryClassification::Mandatory,
                    conditional_on_prior_batch: false,
                },
                ClassificationEntry {
                    identity: facts.m1,
                    role: StorageRole::Record,
                    classification: EntryClassification::Mandatory,
                    conditional_on_prior_batch: false,
                },
                ClassificationEntry {
                    identity: hash_object(b"c5a-o1"),
                    role: StorageRole::Object,
                    classification: EntryClassification::Deferred,
                    conditional_on_prior_batch: false,
                },
            ],
            _ => vec![
                ClassificationEntry {
                    identity: facts.m2,
                    role: StorageRole::Record,
                    classification: EntryClassification::Mandatory,
                    conditional_on_prior_batch: false,
                },
                ClassificationEntry {
                    identity: facts.m3,
                    role: StorageRole::Record,
                    classification: EntryClassification::Mandatory,
                    conditional_on_prior_batch: false,
                },
                ClassificationEntry {
                    identity: facts.m4,
                    role: StorageRole::Record,
                    classification: EntryClassification::Deferred,
                    conditional_on_prior_batch: false,
                },
            ],
        }
    }

    pub(super) fn append_marker(
        builder: &mut ChainBuilder,
        facts: &Facts,
        begin_record: Hash,
        batch: usize,
    ) -> (Hash, ClassificationCounters) {
        let context = BatchCompletionContext {
            mk: facts.mk,
            plan: facts.plan_record,
            batch_index: batch as u64,
            audit: facts.audit_records[batch],
            begin: begin_record,
        };
        let commitment = commit_classification(&context, marker_entries(facts, batch)).unwrap();
        let marker = BatchCompletionMarkerV2 {
            mk: context.mk,
            plan: context.plan,
            batch_index: context.batch_index,
            audit: context.audit,
            begin: context.begin,
            commitment: commitment.digest,
            counters: commitment.counters,
        };
        let record = builder.append(
            Some(reclamation_complete_v2_idempotency_key(
                &facts.mk,
                batch as u64,
            )),
            marker.canonical_bytes(),
        );
        (record, commitment.counters)
    }

    pub(super) fn append_terminal(
        builder: &mut ChainBuilder,
        facts: &Facts,
        begin_record: Hash,
        observations: Vec<RangeCoverageObservation>,
    ) -> Hash {
        let context = facts.expected_context(begin_record);
        let coverage = commit_range_coverage(&context, &observations).unwrap();
        let journal = commit_journal_obligations(&context, &facts.plan.journal_files).unwrap();
        let marker = RangeCompletionMarkerV3 {
            mk: context.mk,
            plan: context.plan,
            begin: context.begin,
            coverage: coverage.digest,
            journal,
            batch_count: observations.len() as u64,
            journal_count: facts.plan.journal_files.len() as u64,
            counters: coverage.aggregate,
        };
        builder.append(
            Some(reclamation_complete_v3_range_idempotency_key(&facts.mk)),
            marker.canonical_bytes(),
        )
    }

    /// A minimal genuine checkpoint boundary whose SK is the
    /// builder's current head (a later checkpoint published on top
    /// of whatever stands there).
    fn append_foreign_checkpoint(builder: &mut ChainBuilder) -> Hash {
        let sk = builder.head;
        let member = hash_object(b"c5a-foreign-member");
        let metadata = CheckpointMetadata::new(
            sk,
            hash_object(b"c5a-foreign-sk-parent"),
            vec![],
            vec![ReclaimedIndexEntry::new(
                Some(b"key-foreign".to_vec()),
                member,
                7,
            )],
            [3u8; 32],
            [5u8; 32],
        );
        builder.append(
            Some(checkpoint_mk_idempotency_key(&sk)),
            metadata.canonical_bytes(),
        )
    }

    /// The genuine campaign: MK → plan → audits → begin → markers →
    /// terminal, with the plan batches [[sk, m1], [m2, m3, m4]] and
    /// the genuine audits.
    struct Campaign {
        builder: ChainBuilder,
        head: Hash,
        terminal: Hash,
        begin_record: Hash,
        marker_records: Vec<Hash>,
        facts: Facts,
    }

    pub(super) fn genuine_batches() -> Vec<PlanBatch> {
        let members = the_members();
        vec![
            PlanBatch {
                index: 0,
                records: vec![members[0], members[1]],
            },
            PlanBatch {
                index: 1,
                records: vec![members[2], members[3], members[4]],
            },
        ]
    }

    fn genuine_campaign() -> Campaign {
        let members = the_members();
        let (audit0_units, audit1_units) = genuine_units(&members);
        let (mut builder, facts) =
            campaign_prefix(genuine_batches(), audit0_units, 1, audit1_units, 1);
        let begin_record = append_begin(&mut builder, &facts);
        let (marker0, counters0) = append_marker(&mut builder, &facts, begin_record, 0);
        let (marker1, counters1) = append_marker(&mut builder, &facts, begin_record, 1);
        let observations = vec![
            RangeCoverageObservation {
                batch_index: 0,
                marker_record: marker0,
                counters: counters0,
            },
            RangeCoverageObservation {
                batch_index: 1,
                marker_record: marker1,
                counters: counters1,
            },
        ];
        let terminal = append_terminal(&mut builder, &facts, begin_record, observations);
        Campaign {
            head: builder.head,
            builder,
            terminal,
            begin_record,
            marker_records: vec![marker0, marker1],
            facts,
        }
    }

    /// The full campaign over mutated prefix parameters (the plan
    /// batches, the audit unit lists, or the reclaimed_keys) — the
    /// consistency adversaries.
    fn mutated_campaign(
        batches: Vec<PlanBatch>,
        audit0_units: Vec<ReclaimUnit>,
        audit0_keys: u64,
        audit1_units: Vec<ReclaimUnit>,
        audit1_keys: u64,
    ) -> (ChainBuilder, Facts, Hash, RangeCompletionContext) {
        let (mut builder, facts) = campaign_prefix(
            batches,
            audit0_units,
            audit0_keys,
            audit1_units,
            audit1_keys,
        );
        let begin_record = append_begin(&mut builder, &facts);
        let (marker0, counters0) = append_marker(&mut builder, &facts, begin_record, 0);
        let (marker1, counters1) = append_marker(&mut builder, &facts, begin_record, 1);
        let observations = vec![
            RangeCoverageObservation {
                batch_index: 0,
                marker_record: marker0,
                counters: counters0,
            },
            RangeCoverageObservation {
                batch_index: 1,
                marker_record: marker1,
                counters: counters1,
            },
        ];
        let head = append_terminal(&mut builder, &facts, begin_record, observations);
        let expected = facts.expected_context(begin_record);
        (builder, facts, head, expected)
    }

    fn observe(
        reads: &mut SyntheticReads,
        head: &Hash,
        expected: &RangeCompletionContext,
    ) -> Result<RangeInstallationObservation, MediumError> {
        verify_range_terminal_on_observed_ancestry(
            reads,
            head,
            expected,
            &RangeInstallationLimits::default(),
        )
    }

    #[test]
    fn the_genuine_observation_reads() {
        let campaign = genuine_campaign();
        let mut reads = campaign.builder.reader();
        let expected = campaign.facts.expected_context(campaign.begin_record);
        let result = observe(&mut reads, &campaign.head, &expected).unwrap();
        let RangeInstallationObservation::ObservedOnAncestry {
            observed_head,
            candidate,
            covering_mk,
            plan_record,
            begin_record,
            batch_markers,
            unresolved,
        } = result
        else {
            panic!("the genuine campaign must observe positively: {result:?}");
        };
        // R1: the result is bound to the exact observation seed.
        assert_eq!(observed_head, campaign.head);
        assert_eq!(candidate.revision, campaign.terminal);
        assert_eq!(covering_mk, campaign.facts.mk);
        assert_eq!(plan_record, campaign.facts.plan_record);
        assert_eq!(begin_record, campaign.begin_record);
        assert_eq!(batch_markers, campaign.marker_records);
        // R8: the exhaustive destructure — every unresolved field a
        // named NotEstablished absence, never a capability; the
        // result holds no authorization, durability, deletion,
        // crossing, or startup field of any kind.
        let UnresolvedObligations {
            provider_canonical_head: NotEstablished,
            begin_durability_authority: NotEstablished,
            founding_derived_seed_authenticity: NotEstablished,
            original_range_digest_historical_correctness: NotEstablished,
            original_object_closures: NotEstablished,
            batch_classification_historical_correctness: NotEstablished,
            physical_retirement: NotEstablished,
            journal_retirement_and_ownership: NotEstablished,
            recovery_convergence: NotEstablished,
            crossing_permission: NotEstablished,
            startup_safety: NotEstablished,
        } = unresolved;
    }

    #[test]
    fn the_expected_mk_terminated_absence() {
        // No terminal published: the complete walk through the
        // evidence to the EXPECTED covering checkpoint establishes
        // the absence (the terminal can only exist above its own
        // MK).
        let members = the_members();
        let (audit0_units, audit1_units) = genuine_units(&members);
        let (mut builder, facts) =
            campaign_prefix(genuine_batches(), audit0_units, 1, audit1_units, 1);
        let begin_record = append_begin(&mut builder, &facts);
        let (marker0, _) = append_marker(&mut builder, &facts, begin_record, 0);
        let (marker1, _) = append_marker(&mut builder, &facts, begin_record, 1);
        let head = builder.head;
        let _ = (marker0, marker1);
        let mut reads = builder.reader();
        let expected = facts.expected_context(begin_record);
        let result = observe(&mut reads, &head, &expected).unwrap();
        let RangeInstallationObservation::NotObservedOnAncestry {
            observed_head,
            terminus: NotObservedTerminus::ExpectedCoveringCheckpoint { mk },
        } = result
        else {
            panic!("the absent terminal must classify: {result:?}");
        };
        assert_eq!(observed_head, head);
        assert_eq!(mk, facts.mk);
    }

    #[test]
    fn the_founding_terminated_absence() {
        // A plain chain with no checkpoint at all: the walk to the
        // founding establishes the absence.
        let mut builder = ChainBuilder::founded("c5a-plain");
        builder.append(None, plain_object(b"pub-1"));
        let head = builder.append(None, plain_object(b"pub-2"));
        let mut reads = builder.reader();
        let expected = RangeCompletionContext {
            mk: hash_object(b"c5a-unrelated-mk"),
            plan: hash_object(b"c5a-unrelated-plan"),
            begin: hash_object(b"c5a-unrelated-begin"),
        };
        let result = observe(&mut reads, &head, &expected).unwrap();
        let RangeInstallationObservation::NotObservedOnAncestry {
            observed_head,
            terminus: NotObservedTerminus::Founding { record },
        } = result
        else {
            panic!("the plain chain must classify: {result:?}");
        };
        assert_eq!(observed_head, head);
        assert!(matches!(
            reads.records.get(&record),
            Some(bytes)
                if matches!(RevisionRecord::decode(bytes), Ok(record) if matches!(record.kind, RecordKind::Founding { .. }))
        ));
    }

    #[test]
    fn the_hidden_behind_foreign_boundary() {
        // A LATER checkpoint published above the terminal: the walk
        // stops at the foreign boundary — absence is NOT claimed
        // (the terminal may exist below it), and the boundary is
        // never crossed.
        let campaign = genuine_campaign();
        let mut builder = campaign.builder;
        let boundary_mk = append_foreign_checkpoint(&mut builder);
        let head = builder.head;
        let mut reads = builder.reader();
        let expected = campaign.facts.expected_context(campaign.begin_record);
        let result = observe(&mut reads, &head, &expected).unwrap();
        let RangeInstallationObservation::HiddenBehindCheckpointBoundary {
            observed_head,
            boundary_mk: observed_boundary,
        } = result
        else {
            panic!("the foreign boundary must classify: {result:?}");
        };
        assert_eq!(observed_head, head);
        assert_eq!(observed_boundary, boundary_mk);
    }

    #[test]
    fn the_ancestry_gap_unresolved() {
        // A mid-segment record removed: the missing parent
        // classifies as the unresolved gap — never an invented
        // skip, never an absence.
        let campaign = genuine_campaign();
        let mut reads = campaign.builder.reader();
        reads.records.remove(&campaign.begin_record);
        let expected = campaign.facts.expected_context(campaign.begin_record);
        let result = observe(&mut reads, &campaign.head, &expected).unwrap();
        let RangeInstallationObservation::AncestryGapUnresolved { observed_head, at } = result
        else {
            panic!("the gap must classify: {result:?}");
        };
        assert_eq!(observed_head, campaign.head);
        assert_eq!(at, campaign.begin_record);
    }

    #[test]
    fn the_off_chain_terminal_never_observed() {
        // The terminal's record and object are in the byte store,
        // but the supplied head's ancestry does not include it
        // (the head is the marker below it): the observation
        // establishes the absence ON THE OBSERVED ANCESTRY — the
        // store presence never succeeds.
        let campaign = genuine_campaign();
        let mut reads = campaign.builder.reader();
        let expected = campaign.facts.expected_context(campaign.begin_record);
        let stale_head = campaign.marker_records[0];
        let result = observe(&mut reads, &stale_head, &expected).unwrap();
        let RangeInstallationObservation::NotObservedOnAncestry {
            observed_head,
            terminus: NotObservedTerminus::ExpectedCoveringCheckpoint { .. },
        } = result
        else {
            panic!("the off-chain terminal must classify: {result:?}");
        };
        assert_eq!(observed_head, stale_head);
    }

    #[test]
    fn the_stale_head_observation_is_head_relative() {
        // A publication continued above the terminal: the
        // observation over the OLDER supplied head still succeeds —
        // the result is bound to the SUPPLIED head (the
        // head-relative contract), never to the store's current
        // top.
        let campaign = genuine_campaign();
        let mut builder = campaign.builder;
        builder.append(None, plain_object(b"later-publication"));
        let mut reads = builder.reader();
        let expected = campaign.facts.expected_context(campaign.begin_record);
        let result = observe(&mut reads, &campaign.terminal, &expected).unwrap();
        let RangeInstallationObservation::ObservedOnAncestry { observed_head, .. } = result else {
            panic!("the stale-head observation must succeed: {result:?}");
        };
        assert_eq!(observed_head, campaign.terminal);
    }

    #[test]
    fn the_evidence_above_terminal_refuses() {
        // A Batch-marker key for the expected MK appended ABOVE the
        // terminal: evidence appearing later on the chain never
        // satisfies an earlier terminal's claim.
        let campaign = genuine_campaign();
        let mut builder = campaign.builder;
        builder.append(
            Some(reclamation_complete_v2_idempotency_key(
                &campaign.facts.mk,
                1,
            )),
            plain_object(b"above-terminal-marker"),
        );
        let head = builder.head;
        let mut reads = builder.reader();
        let expected = campaign.facts.expected_context(campaign.begin_record);
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/evidence-above-terminal"
            })
        ));
    }

    #[test]
    fn the_marker_below_begin_refuses() {
        // A Batch-marker key below the begin: the temporal
        // publication order refuses it.
        let members = the_members();
        let (audit0_units, audit1_units) = genuine_units(&members);
        let (mut builder, facts) =
            campaign_prefix(genuine_batches(), audit0_units, 1, audit1_units, 1);
        // A /2-keyed record BETWEEN the audits and the begin (its
        // payload is irrelevant — the walk refuses at the key). A
        // FRESH batch index (7, not plan-defined) so the temporal
        // rule fires rather than the duplicate-key rule.
        builder.append(
            Some(reclamation_complete_v2_idempotency_key(&facts.mk, 7)),
            plain_object(b"below-begin-marker"),
        );
        let begin_record = append_begin(&mut builder, &facts);
        let (marker0, counters0) = append_marker(&mut builder, &facts, begin_record, 0);
        let (marker1, counters1) = append_marker(&mut builder, &facts, begin_record, 1);
        let observations = vec![
            RangeCoverageObservation {
                batch_index: 0,
                marker_record: marker0,
                counters: counters0,
            },
            RangeCoverageObservation {
                batch_index: 1,
                marker_record: marker1,
                counters: counters1,
            },
        ];
        let head = append_terminal(&mut builder, &facts, begin_record, observations);
        let mut reads = builder.reader();
        let expected = facts.expected_context(begin_record);
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/marker-below-begin"
            })
        ));
    }

    #[test]
    fn the_terminal_before_final_marker_refuses() {
        // The terminal published with only batch 0's marker below
        // it: the marker-set mismatch refuses (evidence later on
        // the chain cannot satisfy the claim).
        let members = the_members();
        let (audit0_units, audit1_units) = genuine_units(&members);
        let (mut builder, facts) =
            campaign_prefix(genuine_batches(), audit0_units, 1, audit1_units, 1);
        let begin_record = append_begin(&mut builder, &facts);
        let (marker0, counters0) = append_marker(&mut builder, &facts, begin_record, 0);
        let observations = vec![RangeCoverageObservation {
            batch_index: 0,
            marker_record: marker0,
            counters: counters0,
        }];
        let head = append_terminal(&mut builder, &facts, begin_record, observations);
        let mut reads = builder.reader();
        let expected = facts.expected_context(begin_record);
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/marker-set-mismatch"
            })
        ));
    }

    #[test]
    fn the_plan_index_partition_refuses() {
        let members = the_members();
        let (audit0_units, audit1_units) = genuine_units(&members);
        // The omitted member: the flattened plan membership is one
        // short of the index's epoch-descending membership.
        let omitted = vec![
            PlanBatch {
                index: 0,
                records: vec![members[0], members[1]],
            },
            PlanBatch {
                index: 1,
                records: vec![members[2], members[3]],
            },
        ];
        let (builder, _, head, expected) =
            mutated_campaign(omitted, audit0_units.clone(), 1, audit1_units.clone(), 1);
        let mut reads = builder.reader();
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/plan-index-partition"
            })
        ));
        // The reordered batch: the first position mismatches.
        let reordered = vec![
            PlanBatch {
                index: 0,
                records: vec![members[1], members[0]],
            },
            PlanBatch {
                index: 1,
                records: vec![members[2], members[3], members[4]],
            },
        ];
        let (builder, _, head, expected) =
            mutated_campaign(reordered, audit0_units, 1, audit1_units, 1);
        let mut reads = builder.reader();
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/plan-index-partition"
            })
        ));
    }

    #[test]
    fn the_audit_consistency_refuses() {
        let members = the_members();
        let (audit0_units, audit1_units) = genuine_units(&members);
        // The reordered units: the ordered unit-record membership
        // mismatches the plan batch.
        let reordered_units = vec![
            ReclaimUnit {
                record: members[1],
                objects: vec![hash_object(b"c5a-o1")],
            },
            ReclaimUnit {
                record: members[0],
                objects: vec![],
            },
        ];
        let (builder, _, head, expected) = mutated_campaign(
            genuine_batches(),
            reordered_units,
            1,
            audit1_units.clone(),
            1,
        );
        let mut reads = builder.reader();
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/audit-membership"
            })
        ));
        // The incorrect reclaimed_keys (recomputed from the index's
        // keyed members: m1 carries a key, so batch 0's count is 1).
        let (builder, _, head, expected) = mutated_campaign(
            genuine_batches(),
            audit0_units.clone(),
            0,
            audit1_units.clone(),
            1,
        );
        let mut reads = builder.reader();
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/audit-reclaimed-keys"
            })
        ));
        // The inconsistent plan membership: audit 1 omits m4.
        let short_units = vec![
            ReclaimUnit {
                record: members[2],
                objects: vec![],
            },
            ReclaimUnit {
                record: members[3],
                objects: vec![],
            },
        ];
        let (builder, _, head, expected) =
            mutated_campaign(genuine_batches(), audit0_units, 1, short_units, 1);
        let mut reads = builder.reader();
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/audit-membership"
            })
        ));
    }

    #[test]
    fn the_corrupt_and_malformed_evidence_refuses() {
        let campaign = genuine_campaign();
        let expected = campaign.facts.expected_context(campaign.begin_record);
        // The corrupted terminal record bytes (the walk's own hash
        // check).
        let mut reads = campaign.builder.reader();
        reads.records.insert(campaign.terminal, b"garbage".to_vec());
        assert!(matches!(
            observe(&mut reads, &campaign.head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/record-hash"
            })
        ));
        // The impostor terminal object bytes (the C4 reader's hash
        // check at completion).
        let mut reads = campaign.builder.reader();
        let terminal_record = RevisionRecord::decode(&reads.records[&campaign.terminal]).unwrap();
        reads
            .objects
            .insert(terminal_record.content_refs[0], b"impostor".to_vec());
        assert!(observe(&mut reads, &campaign.head, &expected).is_err());
        // The corrupted boundary metadata object.
        let mut reads = campaign.builder.reader();
        let mk_record = RevisionRecord::decode(&reads.records[&campaign.facts.mk]).unwrap();
        reads
            .objects
            .insert(mk_record.content_refs[0], b"corrupt-metadata".to_vec());
        assert!(matches!(
            observe(&mut reads, &campaign.head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/mk-object-hash"
            })
        ));
        // The unsupported completion-family variant (a /4 key) and
        // the V1 mixed evidence for the expected MK.
        let mut builder = campaign.builder.clone();
        builder.append(
            Some(
                format!(
                    "engine-persist/reclamation-complete-key/4:{}:range",
                    campaign.facts.mk.hex()
                )
                .into_bytes(),
            ),
            plain_object(b"future-variant"),
        );
        let head = builder.head;
        let mut reads = builder.reader();
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/unsupported-family-variant"
            })
        ));
        let mut builder = campaign.builder.clone();
        builder.append(
            Some(crate::reclaim::reclamation_complete_idempotency_key(
                &campaign.facts.mk,
                None,
            )),
            plain_object(b"v1-range"),
        );
        let head = builder.head;
        let mut reads = builder.reader();
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/v1-mixed-evidence"
            })
        ));
    }

    #[test]
    fn the_context_disagreement_refuses() {
        // The caller's expected plan/begin disagree with the
        // observed segment: the caller-supplied context is never
        // independently authoritative, and a disagreement refuses.
        let campaign = genuine_campaign();
        let mut reads = campaign.builder.reader();
        let expected = RangeCompletionContext {
            mk: campaign.facts.mk,
            plan: hash_object(b"c5a-other-plan"),
            begin: campaign.begin_record,
        };
        assert!(matches!(
            observe(&mut reads, &campaign.head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/context-disagreement"
            })
        ));
    }

    #[test]
    fn the_bound_and_head_failures() {
        let campaign = genuine_campaign();
        let expected = campaign.facts.expected_context(campaign.begin_record);
        // The exhausted traversal bound: a typed refusal, never an
        // absence.
        let mut reads = campaign.builder.reader();
        let result = verify_range_terminal_on_observed_ancestry(
            &mut reads,
            &campaign.head,
            &expected,
            &RangeInstallationLimits { max_records: 3 },
        );
        assert!(matches!(
            result,
            Err(MediumError::Chain {
                rule: "range-observation/bound-exhausted"
            })
        ));
        // The zero limit refuses before any read.
        let mut reads = campaign.builder.reader();
        let result = verify_range_terminal_on_observed_ancestry(
            &mut reads,
            &campaign.head,
            &expected,
            &RangeInstallationLimits { max_records: 0 },
        );
        assert!(matches!(
            result,
            Err(MediumError::Chain {
                rule: "range-observation/zero-limit"
            })
        ));
        // The missing supplied head: a typed refusal, never a gap
        // classification.
        let mut reads = campaign.builder.reader();
        let absent_head = hash_object(b"c5a-nowhere");
        assert!(matches!(
            observe(&mut reads, &absent_head, &expected),
            Err(MediumError::MissingObject { .. })
        ));
    }

    #[test]
    fn the_foreign_evidence_interleaved_is_ordinary() {
        // Unrelated ordinary publications and a FOREIGN checkpoint's
        // Batch marker interleaved in the evidence segment: the
        // observation still succeeds (foreign-MK evidence is
        // ordinary observed content, never interpreted).
        let members = the_members();
        let (audit0_units, audit1_units) = genuine_units(&members);
        let (mut builder, facts) =
            campaign_prefix(genuine_batches(), audit0_units, 1, audit1_units, 1);
        let begin_record = append_begin(&mut builder, &facts);
        builder.append(None, plain_object(b"ordinary-publication"));
        let foreign_mk = hash_object(b"c5a-foreign-mk");
        builder.append(
            Some(reclamation_complete_v2_idempotency_key(&foreign_mk, 0)),
            plain_object(b"foreign-marker"),
        );
        let (marker0, counters0) = append_marker(&mut builder, &facts, begin_record, 0);
        let (marker1, counters1) = append_marker(&mut builder, &facts, begin_record, 1);
        let observations = vec![
            RangeCoverageObservation {
                batch_index: 0,
                marker_record: marker0,
                counters: counters0,
            },
            RangeCoverageObservation {
                batch_index: 1,
                marker_record: marker1,
                counters: counters1,
            },
        ];
        let head = append_terminal(&mut builder, &facts, begin_record, observations);
        let mut reads = builder.reader();
        let expected = facts.expected_context(begin_record);
        let result = observe(&mut reads, &head, &expected).unwrap();
        assert!(matches!(
            result,
            RangeInstallationObservation::ObservedOnAncestry { .. }
        ));
    }

    #[test]
    fn the_foreign_boundary_inside_the_segment_refuses() {
        // A foreign checkpoint boundary BETWEEN the plan and the
        // expected MK (below the plan, above the expected covering
        // MK): the segment is incoherent — the walk refuses rather
        // than crossing or reattributing the boundary.
        let members = the_members();
        let (range_digest, seed) = the_digests();
        let mut builder = ChainBuilder::seeded(members[0], 60);
        let metadata = CheckpointMetadata::new(
            members[0],
            hash_object(b"c5a-sk-parent"),
            vec![],
            the_index(&members),
            range_digest,
            seed,
        );
        let mk = builder.append(
            Some(checkpoint_mk_idempotency_key(&members[0])),
            metadata.canonical_bytes(),
        );
        // The foreign boundary directly above the expected MK.
        append_foreign_checkpoint(&mut builder);
        let plan = ReclaimPlan {
            mk,
            sk: members[0],
            range_digest,
            seed,
            ledger_byte_limit: 2048,
            batches: genuine_batches(),
            journal_files: vec!["journal.t1.0".to_owned(), "journal.t1.2".to_owned()],
        };
        let plan_record = builder.append(
            Some(reclamation_plan_idempotency_key(&mk)),
            plan.canonical_bytes(),
        );
        let (audit0_units, audit1_units) = genuine_units(&members);
        let audit0 = ReclamationLedgerV2 {
            mk,
            sk: members[0],
            batch: 0,
            range_digest,
            seed,
            records: audit0_units.len() as u64,
            reclaimed_keys: 1,
            units: audit0_units,
        };
        let audit0_record = builder.append(
            Some(reclamation_ledger_v2_idempotency_key(&mk, 0)),
            audit0.canonical_bytes(),
        );
        let audit1 = ReclamationLedgerV2 {
            mk,
            sk: members[0],
            batch: 1,
            range_digest,
            seed,
            records: audit1_units.len() as u64,
            reclaimed_keys: 1,
            units: audit1_units,
        };
        let audit1_record = builder.append(
            Some(reclamation_ledger_v2_idempotency_key(&mk, 1)),
            audit1.canonical_bytes(),
        );
        let facts = Facts {
            mk,
            plan,
            plan_record,
            audit_records: vec![audit0_record, audit1_record],
            sk: members[0],
            m1: members[1],
            m2: members[2],
            m3: members[3],
            m4: members[4],
            range_digest,
            seed,
        };
        let begin_record = append_begin(&mut builder, &facts);
        let (marker0, counters0) = append_marker(&mut builder, &facts, begin_record, 0);
        let (marker1, counters1) = append_marker(&mut builder, &facts, begin_record, 1);
        let observations = vec![
            RangeCoverageObservation {
                batch_index: 0,
                marker_record: marker0,
                counters: counters0,
            },
            RangeCoverageObservation {
                batch_index: 1,
                marker_record: marker1,
                counters: counters1,
            },
        ];
        let head = append_terminal(&mut builder, &facts, begin_record, observations);
        let mut reads = builder.reader();
        let expected = facts.expected_context(begin_record);
        assert!(matches!(
            observe(&mut reads, &head, &expected),
            Err(MediumError::Chain {
                rule: "range-observation/foreign-boundary-in-segment"
            })
        ));
    }
}

// ============================================================== the C5b tests
//
// The coordinated-core wrapper's semantics (the synthetic level).
// The full genuine-campaign positives, the concurrency adversaries,
// and the read-only proofs live in the provider files' test suites
// (local.rs and sqlite.rs), where the real coordination scopes and
// the direct-write fixture machinery exist.

#[cfg(test)]
mod c5b_core_tests {
    use super::*;
    use crate::record::AuthorityMode;

    /// A compact record-only synthetic reader (the wrapper-semantics
    /// chains carry no keyed evidence, so no object reads occur).
    struct TinyReads {
        records: std::collections::BTreeMap<Hash, Vec<u8>>,
    }
    impl crate::reclaim::BeginReads for TinyReads {
        fn read_record_bytes(&mut self, id: &Hash) -> Result<Vec<u8>, MediumError> {
            self.records
                .get(id)
                .cloned()
                .ok_or(MediumError::MissingObject { hash: *id })
        }
        fn read_object_bytes(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
            self.records
                .get(hash)
                .cloned()
                .ok_or(MediumError::MissingObject { hash: *hash })
        }
    }

    fn plain_object(payload: &[u8]) -> Vec<u8> {
        let mut enc = engine_core::Encoder::new();
        enc.write_str("sektor7/save/1");
        enc.write_bytes(payload);
        enc.into_bytes()
    }

    /// A founded plain chain of two publications.
    fn tiny_chain() -> (TinyReads, Hash, Hash) {
        let object = plain_object(b"c5b-founding");
        let object_hash = hash_object(&object);
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "c5b-core", 0)
            .with_content_refs(vec![object_hash]);
        let founding_id = founding.id();
        let mut records = std::collections::BTreeMap::new();
        records.insert(founding_id, founding.canonical_bytes());
        let mut head = founding_id;
        for index in 0..2u64 {
            let payload = plain_object(format!("c5b-pub-{index}").as_bytes());
            let payload_hash = hash_object(&payload);
            let record = RevisionRecord::regular(head, index, index + 1)
                .with_content_refs(vec![payload_hash]);
            let id = record.id();
            records.insert(id, record.canonical_bytes());
            head = id;
        }
        (TinyReads { records }, head, founding_id)
    }

    fn an_expected() -> RangeCompletionContext {
        RangeCompletionContext {
            mk: hash_object(b"c5b-unrelated-mk"),
            plan: hash_object(b"c5b-unrelated-plan"),
            begin: hash_object(b"c5b-unrelated-begin"),
        }
    }

    #[test]
    fn the_wrapper_binds_every_result_to_the_coordinated_head() {
        let expected = an_expected();
        let limits = RangeInstallationLimits::default();
        // (a) The complete plain chain: the founding-terminated
        // absence, bound to the coordinated head.
        let (mut reads, head, _) = tiny_chain();
        let coordinated = observe_coordinated(&mut reads, &head, &expected, &limits).unwrap();
        let CoordinatedObservation {
            coordinated_head,
            observation,
            unresolved: _,
        } = &coordinated;
        assert_eq!(*coordinated_head, head);
        match observation {
            RangeInstallationObservation::NotObservedOnAncestry {
                observed_head,
                terminus: NotObservedTerminus::Founding { .. },
            } => assert_eq!(*observed_head, *coordinated_head),
            other => panic!("the plain chain must classify: {other:?}"),
        }
        // (b) The mid-chain record removed: the unresolved gap,
        // bound to the coordinated head (never an absence).
        let (mut reads, head, founding) = tiny_chain();
        reads.records.remove(&founding);
        let coordinated = observe_coordinated(&mut reads, &head, &expected, &limits).unwrap();
        let CoordinatedObservation {
            coordinated_head,
            observation,
            unresolved: _,
        } = &coordinated;
        assert_eq!(*coordinated_head, head);
        match observation {
            RangeInstallationObservation::AncestryGapUnresolved { observed_head, at } => {
                assert_eq!(*observed_head, *coordinated_head);
                assert_eq!(*at, founding);
            }
            other => panic!("the gap must classify: {other:?}"),
        }
    }

    #[test]
    fn the_wrapper_validates_inputs_before_any_read() {
        let expected = an_expected();
        let (mut reads, head, _) = tiny_chain();
        // The zero limit refuses before any read.
        assert!(matches!(
            observe_coordinated(
                &mut reads,
                &head,
                &expected,
                &RangeInstallationLimits { max_records: 0 }
            ),
            Err(MediumError::Chain {
                rule: "range-observation/zero-limit"
            })
        ));
        // The placeholder expected context refuses through the C5a
        // step-0 validation.
        let invalid = RangeCompletionContext {
            mk: Hash::ZERO,
            plan: hash_object(b"c5b-plan"),
            begin: hash_object(b"c5b-begin"),
        };
        assert!(matches!(
            observe_coordinated(
                &mut reads,
                &head,
                &invalid,
                &RangeInstallationLimits::default()
            ),
            Err(MediumError::Format(_))
        ));
    }

    #[test]
    fn the_coordinated_unresolved_destructure_names_every_field() {
        // R8 at the coordinated level: the exhaustive destructure —
        // twelve named NotEstablished absences (the eleven C5a
        // obligations PLUS the head-currency limitation), never a
        // capability.
        let CoordinatedUnresolvedObligations {
            provider_canonical_head: NotEstablished,
            begin_durability_authority: NotEstablished,
            founding_derived_seed_authenticity: NotEstablished,
            original_range_digest_historical_correctness: NotEstablished,
            original_object_closures: NotEstablished,
            batch_classification_historical_correctness: NotEstablished,
            physical_retirement: NotEstablished,
            journal_retirement_and_ownership: NotEstablished,
            recovery_convergence: NotEstablished,
            crossing_permission: NotEstablished,
            startup_safety: NotEstablished,
            head_currency_beyond_scope: NotEstablished,
        } = CoordinatedUnresolvedObligations::all();
    }

    #[test]
    fn the_coordinated_type_exposes_exactly_three_fields() {
        // The type-level proof: the coordinated observation holds
        // exactly the coordinated head, the C5a observation, and the
        // unresolved obligations — no verdict, capability, or
        // certificate field exists.
        let expected = an_expected();
        let (mut reads, head, _) = tiny_chain();
        let coordinated = observe_coordinated(
            &mut reads,
            &head,
            &expected,
            &RangeInstallationLimits::default(),
        )
        .unwrap();
        let CoordinatedObservation {
            coordinated_head,
            observation,
            unresolved,
        } = coordinated;
        assert_eq!(coordinated_head, head);
        assert!(matches!(
            observation,
            RangeInstallationObservation::NotObservedOnAncestry { .. }
        ));
        assert_eq!(unresolved, CoordinatedUnresolvedObligations::all());
    }
}

#[cfg(test)]
mod c5d_core_tests {
    use super::c5a_tests::{
        ChainBuilder, append_begin, append_marker, append_terminal, campaign_prefix,
        genuine_batches, genuine_units, plain_object, the_digests, the_index, the_members,
    };
    use super::*;
    use crate::range_completion::RangeCoverageObservation;
    use crate::reclaim::{ReclamationBegin, reclamation_begin_idempotency_key};
    use crate::recovery_discovery::{DiscoveryLimits, discover_recovery_evidence};

    /// Runs the discovery over the builder's chain from its head
    /// and applies the derivation — stages one and two of the
    /// composite over the synthetic reader.
    fn derive(builder: &ChainBuilder) -> DiscoveredRangeContext {
        let mut reads = builder.reader();
        let discovery =
            discover_recovery_evidence(&mut reads, &builder.head, &DiscoveryLimits::default())
                .unwrap();
        derive_range_completion_context(&discovery)
    }

    /// The genuine prefix arguments (the C5a fixture's own).
    fn genuine_prefix() -> (ChainBuilder, crate::range_installation::c5a_tests::Facts) {
        let members = the_members();
        let (audit0_units, audit1_units) = genuine_units(&members);
        campaign_prefix(genuine_batches(), audit0_units, 1, audit1_units, 1)
    }

    #[test]
    fn the_genuine_campaign_derives_the_full_context() {
        let (mut builder, facts) = genuine_prefix();
        let begin = append_begin(&mut builder, &facts);
        let (marker0, counters0) = append_marker(&mut builder, &facts, begin, 0);
        let (marker1, counters1) = append_marker(&mut builder, &facts, begin, 1);
        let observations = vec![
            RangeCoverageObservation {
                batch_index: 0,
                marker_record: marker0,
                counters: counters0,
            },
            RangeCoverageObservation {
                batch_index: 1,
                marker_record: marker1,
                counters: counters1,
            },
        ];
        let _terminal = append_terminal(&mut builder, &facts, begin, observations);
        // The derived context is EXACTLY the fixture's expected
        // context — the derivation's three identities (the covering
        // MK, the plan, the begin) are the same facts the C5a
        // verifier's callers supply by hand.
        match derive(&builder) {
            DiscoveredRangeContext::V2Context {
                context,
                covering_mk,
                records_consulted,
            } => {
                assert_eq!(context, facts.expected_context(begin));
                assert_eq!(covering_mk, facts.mk);
                assert!(records_consulted > 0);
            }
            other => panic!("the genuine campaign must derive (a non-V2 ladder result: {other:?})"),
        }
    }

    #[test]
    fn the_in_flight_range_without_a_terminal_still_derives() {
        // The startup's live case: the reclamation began (the plan
        // and the begin published, the batch markers landing) but
        // the /3 terminal is not yet published. The derivation
        // needs only the MK, the plan, and the begin — the terminal
        // is the OBSERVATION stage's subject, never a derivation
        // input.
        let (mut builder, facts) = genuine_prefix();
        let begin = append_begin(&mut builder, &facts);
        let _ = append_marker(&mut builder, &facts, begin, 0);
        match derive(&builder) {
            DiscoveredRangeContext::V2Context { context, .. } => {
                assert_eq!(context, facts.expected_context(begin));
            }
            other => panic!("the in-flight range must derive (a non-V2 ladder result: {other:?})"),
        }
    }

    #[test]
    fn the_segment_without_a_begin_classifies() {
        let (builder, facts) = genuine_prefix();
        match derive(&builder) {
            DiscoveredRangeContext::NoBeginAbovePlan { covering_mk, plan } => {
                assert_eq!(covering_mk, facts.mk);
                assert_eq!(plan, facts.plan_record);
            }
            other => panic!("the begin-less segment must classify (got: {other:?})"),
        }
    }

    #[test]
    fn the_segment_without_a_plan_classifies() {
        // The covering MK alone: no V2 reclamation was ever begun
        // above it.
        let members = the_members();
        let (range_digest, seed) = the_digests();
        let mut builder = ChainBuilder::seeded(members[0], 60);
        let metadata = CheckpointMetadata::new(
            members[0],
            hash_object(b"c5d-sk-parent"),
            vec![],
            the_index(&members),
            range_digest,
            seed,
        );
        let mk = builder.append(
            Some(checkpoint_mk_idempotency_key(&members[0])),
            metadata.canonical_bytes(),
        );
        match derive(&builder) {
            DiscoveredRangeContext::NoPlanAboveCovering { covering_mk } => {
                assert_eq!(covering_mk, mk);
            }
            other => panic!("the plan-less segment must classify (got: {other:?})"),
        }
    }

    #[test]
    fn the_founded_chain_without_any_checkpoint_classifies() {
        let mut builder = ChainBuilder::founded("c5d-core");
        let founding = builder.head;
        let _ = builder.append(None, plain_object(b"c5d-pub-0"));
        let _ = builder.append(None, plain_object(b"c5d-pub-1"));
        match derive(&builder) {
            DiscoveredRangeContext::NoCoveringCheckpoint { founding: observed } => {
                assert_eq!(observed, founding);
            }
            other => panic!("the checkpoint-less chain must classify (got: {other:?})"),
        }
    }

    #[test]
    fn the_v1_begin_shape_classifies() {
        // A begin whose source binds an AUDIT revision (the V1
        // shape) — the V2 Range-terminal observation does not
        // apply; the derivation names the evidence for the existing
        // V1 verifiers.
        let (mut builder, facts) = genuine_prefix();
        let v1_begin = ReclamationBegin {
            mk: facts.mk,
            source: facts.audit_records[0],
            range_digest: facts.range_digest,
            seed: facts.seed,
        };
        let begin = builder.append(
            Some(reclamation_begin_idempotency_key(&facts.mk)),
            v1_begin.canonical_bytes(),
        );
        match derive(&builder) {
            DiscoveredRangeContext::V1BeginObserved {
                covering_mk,
                plan,
                begin: observed_begin,
            } => {
                assert_eq!(covering_mk, facts.mk);
                assert_eq!(plan, facts.plan_record);
                assert_eq!(observed_begin, begin);
            }
            other => panic!("the V1 begin shape must classify (got: {other:?})"),
        }
    }

    #[test]
    fn the_composite_types_expose_exactly_the_documented_fields() {
        // The type-level proof: the composite result holds the
        // derivation ladder and — only for the derived V2 context —
        // the coordinated observation. No verdict, capability, or
        // certificate field exists on either type.
        let result = DiscoveredRangeObservation {
            derivation: DiscoveredRangeContext::NoPlanAboveCovering {
                covering_mk: Hash::ZERO,
            },
            observation: None,
        };
        let DiscoveredRangeObservation {
            derivation,
            observation,
        } = result;
        assert!(matches!(
            derivation,
            DiscoveredRangeContext::NoPlanAboveCovering { .. }
        ));
        assert!(observation.is_none());
        let full = DiscoveredRangeObservation {
            derivation: DiscoveredRangeContext::V2Context {
                context: RangeCompletionContext {
                    mk: Hash::ZERO,
                    plan: Hash::ZERO,
                    begin: Hash::ZERO,
                },
                covering_mk: Hash::ZERO,
                records_consulted: 1,
            },
            observation: None,
        };
        let DiscoveredRangeObservation {
            derivation,
            observation,
        } = full;
        let DiscoveredRangeContext::V2Context {
            context,
            covering_mk: _,
            records_consulted: _,
        } = derivation
        else {
            panic!("the constructed V2 ladder must destructure");
        };
        assert_eq!(context.mk, Hash::ZERO);
        assert!(observation.is_none());
    }
}
