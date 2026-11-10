//! (V2X-CP3-A3) THE EXACT PRESERVATION AND RETIREMENT-PARTITION
//! ACCOUNTING — the read-only, bounded, streaming evaluator that
//! determines, for ONE plan-defined batch of ONE authenticated
//! checkpoint, the exact identity relationships governing what must
//! survive a prospective reclamation execution.
//!
//! **The evidence base (immutable, authenticated, reused — never
//! duplicated or weakened):** the accounting acquires every fact
//! through the accepted readers. The CP2 plan-bound begin authority
//! is re-established FIRST through the medium's own
//! [`CanonicalMedium::verify_begin_authority_v2`] (the freshness
//! discipline — a previously verified state is not a standing
//! storage guarantee); the checkpoint MK is authenticated by the
//! canonical descent match with its permanent metadata; the plan is
//! discovered through the deterministic MK-bound key; every
//! plan-listed batch audit is located by the single-descent
//! multi-key discovery (O(chain) once — the CP1 facility); the
//! historical range is reconstructed by the A2 authenticated
//! walker (the witness preimages for the members, the live records
//! for the retained anchors); and every survivor's composite
//! dependencies are established through the accepted
//! closure-verification reader. No parallel or weaker verifier
//! exists here.
//!
//! **The candidate authority (the immutable V2 audit):** the
//! selected batch's Mandatory/Deferred candidate universe derives
//! EXCLUSIVELY from its authenticated, immutable
//! [`ReclamationLedgerV2`] — the record identity and the
//! closure-expanded object manifest per unit, verified against the
//! plan's ordered batch membership, the checkpoint's digest and
//! seed bindings, the recorded ledger byte limit, and the strict
//! ratified decode (the sharing rules, the uniqueness and count
//! coherence). The accounting NEVER reconstructs the deletion
//! manifest by rereading original retirement records that may
//! already be absent — physical storage observations are never a
//! substitute for the installed audit.
//!
//! **The complete protected-root universe (three regions, one
//! chain, no legacy skips):** the accounting protects every
//! authenticated surviving root relevant to the selected batch —
//! (1) the live canonical revisions from the HEAD down to and
//! including the MK (the checkpoint seals, the plan, every audit,
//! the begin, the witness publications — read parent-by-parent
//! with hash-verifying reads, NO skip tables: an absent record in
//! this region refuses typed rather than being silently crossed);
//! (2) the checkpoint range reconstructed by the A2 walker — every
//! retained anchor and every FUTURE-batch member is a survivor
//! root (its record identity, its content references, and its
//! verified composite closure), while the CURRENT-batch members
//! are the candidates and the PRIOR-batch members are neither
//! (see the premise below); and (3) the older surviving history
//! below the authenticated boundary — when the boundary is a
//! prior checkpoint MK, every record from the boundary's parent
//! down to the campaign founding is walked live and protected
//! (record, references, and closure); an absent record there
//! refuses typed (the region is never silently treated as empty).
//! The three regions are disjoint and together cover the whole
//! canonical chain relevant to this checkpoint's authority. The
//! A3 descents never use a legacy checkpoint skip: a skip that
//! would silently exclude a live future-batch member, a retained
//! anchor, or a below-boundary record is unrepresentable here.
//!
//! **The prior-batch premise (the completion-receipt trust policy
//! remains UNRATIFIED):** a PRIOR-batch member is not a surviving
//! root. Its retirement is sequenced by THIS plan's own batch
//! ordering — the only cross-batch sequencing this checkpoint can
//! name — so an audited identity that a prior batch's own audit
//! also covers is classified Mandatory with the explicit
//! per-identity `conditional_on_prior_batches` flag, and the
//! result carries the batch-level
//! [`PriorBatchPremise::ConditionalOnPriorBatches`] naming the
//! premise. This is a representation of dependence, NEVER a
//! completion claim: the physical absence of a prior member's
//! record changes nothing (absence is not completion authority),
//! and no caller-supplied marker ever becomes one. Below the
//! boundary the treatment is deliberately CONSERVATIVE: no
//! cross-checkpoint execution ordering is ratified, so every
//! present below-boundary record is protected as it stands (its
//! retirement, if any, is the prior checkpoint's own campaign's
//! business — a separate accounting).
//!
//! **The record/object namespace collision (the conservative
//! protection):** the providers do not agree on the physical
//! namespace — the LocalFile store keeps records and ordinary
//! objects in ONE content-addressed directory (a record's
//! canonical preimage IS an object file), while SQLite keeps
//! separate `records` and `objects` tables. When a candidate
//! identity in one role coincides with a surviving root's
//! requirement on the SAME hash in the OTHER role, the accounting
//! never produces contradictory physical obligations on the same
//! bytes: the identity is classified Deferred — the bytes are
//! conservatively PROTECTED — and the coincidence is recorded in
//! the result's `namespace_collisions` with both roles and the
//! protecting roots, so a future destructive consumer can apply
//! its provider's actual namespace semantics (the SQLite split
//! tables) with the conflict visible instead of hidden. The same
//! hash in the SAME role is the ordinary shared-object protection
//! (Deferred with attribution). A candidate record identity that
//! coincides with a surviving root's RECORD identity refuses
//! typed — the authenticated exclusion's internal-consistency
//! invariant, the accepted partition's own rule.
//!
//! **The global-first-seen discipline (the ratified behavior):**
//! an identity repeated across units appears in the result exactly
//! once, attributed to the FIRST unit in the audit's persisted
//! order that references it; the `shared_references`
//! classification carries every referencing unit. The external
//! sort's total deterministic key (identity, role, source) makes
//! the classification independent of the spill-run boundaries — a
//! different iteration order cannot change the canonical result.
//! The output lists are exact identity sets in the canonical
//! sorted order, never counts or digests: a consumer verifies
//! membership identity-by-identity through
//! [`RetirementAccounting::classification_of`].
//!
//! **The resource contract (bounded, streaming, exact):** the
//! computation retains at most one audit's payload, one plan, one
//! checkpoint metadata, one survivor record's reference working
//! set, and the caller's entry buffer; every identity stream
//! (candidates, survivor requirements, prior-batch references)
//! spills through framed scratch runs (fixed 66-byte entries)
//! merged by a deterministic k-way pass, and the walk-emitted
//! survivor references spool through a dedicated scratch stream
//! (never an in-memory aggregate). Time is O(E log E) over the E
//! accounted identity entries (the per-run sorts plus the merge),
//! and scratch space is O(E × 66) bytes under the caller's
//! ceilings. THE PEAK MEMORY, INCLUDING THE OUTPUTS: the entry
//! buffer (≤ [`MAX_ACCOUNTING_RUN_BUFFER_ENTRIES`]), the bounded
//! root-index lists (the plan-scale future/anchor/current
//! identities), one working set at a time, and the MATERIALIZED
//! RESULT LISTS — each bounded by its own caller ceiling with a
//! typed refusal on crossing (the Mandatory list ≤
//! [`AccountingLimits::max_mandatory_identities`], the Deferred
//! list ≤ [`AccountingLimits::max_deferred_identities`], the
//! shared and collision lists likewise, the per-deferred
//! protector attribution ≤
//! [`AccountingLimits::max_protectors_per_deferred`]); the
//! defaults sit above the 512 MiB ledger ceiling's implied
//! identity bound, so every ledger-bounded audit fits them, and
//! an over-ceiling state REFUSES typed rather than truncating or
//! weakening the exact result.
//!
//! **The scratch transport integrity (non-authoritative in
//! practice, not only in intent):** the scratch is temporary
//! state that can never silently substitute, omit, duplicate, or
//! reorder evidence. Every run and the survivor-reference spool
//! carry a KEYED SHA-256 trailer MAC — the key is generated
//! in-process from the operating system's entropy and never
//! touches the disk, so a file rewritten between the spill and
//! the merge (even with valid framing and counts) fails the
//! recomputed MAC and refuses typed; each run's MAC binds its
//! POSITION (a reordered or renamed run file fails its own
//! position's recomputation); every consumed run is validated as
//! exactly the bytes the trusted computation wrote, in strictly
//! sorted order, under exactly-framed fixed-size entries; the
//! working directory is created EXCLUSIVELY (a preexisting
//! directory name refuses), each run file is created with
//! create-new semantics, and the merge refuses any unexpected
//! entry in the workspace. The MAC is a TRANSPORT GUARD, never a
//! classification input: the exact identity equality stands on
//! the decoded entries themselves, and no scratch count or
//! digest ever substitutes for it. The scratch is removed on
//! completion (a cleanup failure is recorded, never silently
//! ignored, and never converts into an unsafe success claim) and
//! best-effort on EVERY failing path.
//! The descents are a constant number of chain walks (the begin
//! authority, the plan discovery, the audit-locator discovery,
//! the A2 walk, the live-chain descent, the below-boundary
//! descent) — never a per-identity walk.
//!
//! **The read-only truth (no destructive authority of any
//! kind):** the accounting performs NO mutation — no appends, no
//! HEAD movement, no record or object deletion, no journal
//! change, no marker publication, no provider authority change.
//! The result NEVER contains an unqualified deletion permission:
//! there is no `safe_to_delete` claim, no executable permit, no
//! durable certificate. The accounting's physical reads are
//! ORDINARY independent reads — no provider coordination boundary
//! (the LocalFile HEAD-lock discipline or a SQLite transaction)
//! is held across them, and an unchanged HEAD alone does not
//! certify that physical storage remained stable; the result's
//! [`AccountingLimitations`] states the missing coordination
//! requirement explicitly for the future destructive consumer
//! that must establish it. Logical retirement accounting is not
//! automatically proof of safe physical deletion.

use crate::hash::Hash;
use crate::medium::{CanonicalMedium, MediumError, StoredObject};
use crate::reclaim::{
    BeginReads, MAX_RECLAMATION_LEDGER_BYTES, MediumReads, ReclamationLedgerV2,
    discover_keyed_set_via, find_installed_plan_via, reclamation_ledger_v2_idempotency_key,
};
use sha2::{Digest, Sha256};
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};

/// The descent bound for the accounting's own parent-by-parent
/// walks (the live chain and the below-boundary history) — the
/// same magnitude as the accepted walkers' bounds.
const ACCOUNTING_DESCENT_BOUND: u64 = 1 << 20;

// ============================================================ the result

/// The physical storage role of an accounted identity — the
/// record/object namespace distinction the exact accounting must
/// preserve (a revision record's identity and an ordinary content
/// object's identity may coincide as hashes while remaining
/// distinct obligations under a provider's namespace semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageRole {
    /// A revision record's canonical identity.
    Record,
    /// An ordinary content object's identity.
    Object,
}

/// The class of surviving root that protects an accounted
/// identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtectorKind {
    /// A retained anchor inside the reconstructed checkpoint
    /// range (unindexed on the original ancestry — never a
    /// retirement member).
    RangeAnchor,
    /// A FUTURE-batch retirement member — live for this
    /// evaluation's safety analysis (its own batch has not
    /// executed).
    FutureBatchMember,
    /// A canonical revision on the live HEAD-to-MK chain (the
    /// checkpoint seals and every later persistent evidence
    /// publication).
    LiveChainRevision,
    /// The authenticated traversal boundary — the prior
    /// checkpoint's MK or the campaign founding.
    RangeBoundary,
    /// A record on the older surviving history below the
    /// boundary — conservatively protected (no cross-checkpoint
    /// execution ordering is ratified).
    BelowBoundaryHistory,
}

/// One protecting surviving root: its class and its identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProtectorRef {
    /// The survivor class.
    pub kind: ProtectorKind,
    /// The surviving root's identity.
    pub root: Hash,
}

/// The prior-batch completion premise the Mandatory partition
/// carries — the truthful representation of the unratified
/// completion-receipt trust policy (a dependence, never a
/// completion claim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorBatchPremise {
    /// The selected batch is the plan's first — no prior batch
    /// exists, and the Mandatory partition is unconditional.
    FirstBatch,
    /// The Mandatory partition is CONDITIONAL on the listed prior
    /// plan batches' retirements having executed — an unverified
    /// historical premise represented explicitly, never
    /// certified. The plan's own batch ordering is the only
    /// cross-batch sequencing this checkpoint can name.
    ConditionalOnPriorBatches {
        /// The prior batch indices, ascending.
        batches: Vec<u64>,
    },
}

/// A Mandatory identity: audited for retirement, with NO
/// authenticated surviving obligation on its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MandatoryIdentity {
    /// The identity.
    pub identity: Hash,
    /// The storage role under which the audit retires it.
    pub role: StorageRole,
    /// The FIRST unit (in the audit's persisted order) that
    /// references the identity — the ratified global-first-seen
    /// attribution.
    pub first_seen_unit: u64,
    /// Whether a prior batch's own audit also covers this
    /// identity — the per-identity form of the prior-batch
    /// premise (its safe removal presumes that batch's
    /// retirement executed).
    pub conditional_on_prior_batches: bool,
}

/// A Deferred identity: audited for retirement, but authenticated
/// surviving roots require its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredIdentity {
    /// The identity.
    pub identity: Hash,
    /// The storage role under which the audit retires it.
    pub role: StorageRole,
    /// The FIRST unit (in the audit's persisted order) that
    /// references the identity.
    pub first_seen_unit: u64,
    /// Every protecting surviving root (sorted, deduplicated) —
    /// the exact attribution of the protection.
    pub protectors: Vec<ProtectorRef>,
    /// Whether a protecting requirement arrives in the OTHER
    /// storage role on the same hash — the record/object
    /// namespace collision (see [`NamespaceCollision`]).
    pub namespace_collision: bool,
}

/// A shared reference: one identity referenced by two or more of
/// the selected batch's retiring units (accounted once, under the
/// first-seen rule, with every referencing unit recorded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedReference {
    /// The identity.
    pub identity: Hash,
    /// The storage role.
    pub role: StorageRole,
    /// Every referencing unit index, ascending.
    pub referencing_units: Vec<u64>,
}

/// A record/object namespace collision: a candidate identity in
/// one storage role coincides with a surviving root's requirement
/// on the SAME hash in the other role. The bytes are conservatively
/// protected (Deferred) because the LocalFile provider stores
/// records and objects in one physical namespace — a removal
/// obligation would contradict the protection. The classification
/// records both roles and the protecting roots so a future
/// destructive consumer can apply its provider's actual namespace
/// semantics (SQLite's split tables) with the conflict visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceCollision {
    /// The coinciding identity.
    pub identity: Hash,
    /// The role under which the audit retires the identity.
    pub candidate_role: StorageRole,
    /// The FIRST unit referencing the identity in the candidate
    /// role.
    pub first_seen_unit: u64,
    /// The surviving roots requiring the same hash in the OTHER
    /// role (sorted, deduplicated).
    pub protectors: Vec<ProtectorRef>,
    /// The collision's resolution.
    pub classification: CollisionClassification,
}

/// The collision resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionClassification {
    /// The bytes are conservatively PROTECTED: the identity is
    /// classified Deferred with the collision recorded. This is
    /// the safe uniform treatment across the providers; a future
    /// destructive consumer may resolve the record-role removal
    /// separately under a provably split namespace (SQLite's
    /// separate tables) using this visible classification.
    ConservativelyProtected,
}

/// The verified survivor-root summary (bounded metadata — counts
/// and identities, never the reference sets themselves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurvivorRootSummary {
    /// The retained anchors inside the reconstructed range.
    pub range_anchors: u64,
    /// The FUTURE-batch members (live for this evaluation).
    pub future_batch_members: u64,
    /// The live HEAD-to-MK revisions (inclusive of the MK).
    pub live_chain_revisions: u64,
    /// The below-boundary revisions walked and protected (zero
    /// when the boundary is the founding).
    pub below_boundary_revisions: u64,
    /// The authenticated boundary record.
    pub boundary: Hash,
    /// Whether the boundary is a prior checkpoint MK (true) or
    /// the campaign founding (false).
    pub boundary_prior_checkpoint: bool,
    /// The survivor roots whose verified composite closure
    /// expanded beyond their direct content references.
    pub composite_closure_roots: u64,
    /// The total protector-stream entries pushed (records and
    /// objects) — the survivor side's exact streamed size.
    pub survivor_requirement_entries: u64,
}

/// The exact counts, coherent with the identity lists by
/// construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccountingCounts {
    /// The selected batch's audited unit count.
    pub audit_units: u64,
    /// The distinct candidate (identity, role) pairs — the
    /// complete audited candidate membership.
    pub candidate_identities: u64,
    /// The Mandatory count.
    pub mandatory: u64,
    /// The Deferred count.
    pub deferred: u64,
    /// The Mandatory identities carrying the prior-batch
    /// conditional flag.
    pub conditional: u64,
    /// The cross-unit shared references.
    pub shared_references: u64,
    /// The record/object namespace collisions.
    pub namespace_collisions: u64,
}

/// The provider-coordination fact the accounting did NOT
/// establish (a future destructive consumer must).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveCoordination {
    /// The accounting performed ordinary independent reads: no
    /// single coordination boundary (the LocalFile HEAD-lock
    /// discipline or a SQLite transaction) was held across them.
    /// An unchanged HEAD alone does not certify that physical
    /// storage remained stable; the observations are evidence
    /// gathered at read time, never a standing live-state
    /// certification.
    OrdinaryReadsWithoutCoordinationBoundary,
}

/// The below-boundary region's scope treatment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeeperHistoryScope {
    /// The authenticated boundary IS the campaign founding — no
    /// older history exists.
    FoundingBoundary,
    /// Every record below the prior-checkpoint boundary was
    /// walked live and conservatively protected, down to the
    /// founding.
    BelowBoundaryWalked { revisions: u64 },
}

/// The completion-receipt trust policy status carried in the
/// result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorBatchCompletionTrust {
    /// UNRATIFIED — the prior-batch premise is represented, never
    /// certified.
    Unratified,
}

/// The explicit assumptions and limitations of the accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountingLimitations {
    /// The live-state coordination the accounting did not
    /// establish.
    pub live_coordination: LiveCoordination,
    /// The below-boundary region's treatment.
    pub deeper_history: DeeperHistoryScope,
    /// The completion-receipt trust policy status.
    pub prior_batch_completion_trust: PriorBatchCompletionTrust,
}

/// The scratch computation's report (non-authoritative temporary
/// state — recorded, never trusted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchReport {
    /// The spill runs written.
    pub runs_written: u64,
    /// The scratch bytes written.
    pub bytes_written: u64,
    /// The identity entries written across all runs.
    pub entries_written: u64,
    /// Whether the scratch directory could NOT be fully removed
    /// (a recorded cleanup failure — never an unsafe success
    /// claim).
    pub cleanup_residual: bool,
}

/// The classification of one accounted identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// Mandatory (subject to the prior-batch premise flag).
    Mandatory {
        /// The per-identity prior-batch conditional flag.
        conditional_on_prior_batches: bool,
    },
    /// Deferred (protected by the exact surviving roots).
    Deferred {
        /// The protecting roots.
        protectors: Vec<ProtectorRef>,
        /// Whether a protector requirement arrives in the other
        /// storage role on the same hash.
        namespace_collision: bool,
    },
}

/// The exact, bounded, read-only preservation accounting for one
/// plan-defined batch of one authenticated checkpoint. The result
/// is deterministic (the global-first-seen discipline over the
/// total identity order), exact (identity-by-identity lists,
/// never counts or digests), and carries its own premises and
/// limitations. It contains NO deletion permission of any kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetirementAccounting {
    /// The source checkpoint.
    pub source_mk: Hash,
    /// The selected evaluation batch.
    pub selected_batch: u64,
    /// The plan's batch count.
    pub plan_batch_count: u64,
    /// The verified CP2 begin revision's identity.
    pub begin_revision: Hash,
    /// The plan revision's canonical identity.
    pub plan_revision: Hash,
    /// The selected batch's audit revision identity.
    pub audit_revision: Hash,
    /// The selected batch's audit payload object identity.
    pub audit_object: Hash,
    /// The plan's recorded ledger byte limit.
    pub ledger_byte_limit: u64,
    /// The checkpoint's pre-verified range digest.
    pub range_digest: [u8; 32],
    /// The digest chain's founding-derived seed.
    pub founding_seed: [u8; 32],
    /// The exact Mandatory partition (sorted by identity, role).
    pub mandatory: Vec<MandatoryIdentity>,
    /// The exact Deferred partition (sorted by identity, role).
    pub deferred: Vec<DeferredIdentity>,
    /// The cross-unit shared references (sorted).
    pub shared_references: Vec<SharedReference>,
    /// The record/object namespace collisions (sorted).
    pub namespace_collisions: Vec<NamespaceCollision>,
    /// The prior-batch premise.
    pub prior_batch_premise: PriorBatchPremise,
    /// The survivor-root summary.
    pub survivors: SurvivorRootSummary,
    /// The exact counts.
    pub counts: AccountingCounts,
    /// The explicit assumptions and limitations.
    pub limitations: AccountingLimitations,
    /// The scratch computation's report.
    pub scratch: ScratchReport,
}

impl RetirementAccounting {
    /// The exact membership classification of one (identity,
    /// role) pair — None when the identity is not a candidate of
    /// the accounted batch. A consumer verifies exact membership
    /// through this, never through a count or digest.
    pub fn classification_of(&self, identity: &Hash, role: StorageRole) -> Option<Classification> {
        let position = |list: &[MandatoryIdentity]| {
            list.partition_point(|entry| (entry.identity, entry.role) < (*identity, role))
        };
        let index = position(&self.mandatory);
        if let Some(entry) = self.mandatory.get(index)
            && entry.identity == *identity
            && entry.role == role
        {
            return Some(Classification::Mandatory {
                conditional_on_prior_batches: entry.conditional_on_prior_batches,
            });
        }
        let index = self
            .deferred
            .partition_point(|entry| (entry.identity, entry.role) < (*identity, role));
        if let Some(entry) = self.deferred.get(index)
            && entry.identity == *identity
            && entry.role == role
        {
            return Some(Classification::Deferred {
                protectors: entry.protectors.clone(),
                namespace_collision: entry.namespace_collision,
            });
        }
        None
    }

    /// Whether the (identity, role) pair is a candidate of the
    /// accounted batch.
    pub fn contains(&self, identity: &Hash, role: StorageRole) -> bool {
        self.classification_of(identity, role).is_some()
    }
}

/// The hard maximum for the in-memory entry buffer (the spill
/// threshold): 2^20 entries (~73 MiB of entries) — the largest
/// spill buffer the implementation supports.
pub const MAX_ACCOUNTING_RUN_BUFFER_ENTRIES: usize = 1 << 20;

/// The hard maximum for the spill-run count (the merge fan-in):
/// 2^16 runs.
pub const MAX_ACCOUNTING_RUN_COUNT: usize = 1 << 16;

/// The hard maximum for the total scratch bytes: 2^36.
pub const MAX_ACCOUNTING_SCRATCH_BYTE_CEILING: u64 = 1 << 36;

/// The hard maximum for each materialized classified list (the
/// Mandatory list and the Deferred list separately): 2^24
/// entries — above the 512 MiB ledger byte ceiling's implied
/// identity bound (~13.4 M), so every ledger-bounded audit fits.
pub const MAX_ACCOUNTING_CLASSIFIED_LIST: u64 = 1 << 24;

/// The hard maximum for the materialized shared-reference list:
/// 2^24 entries.
pub const MAX_ACCOUNTING_SHARED_LIST: u64 = 1 << 24;

/// The hard maximum for the materialized namespace-collision
/// list: 2^24 entries.
pub const MAX_ACCOUNTING_COLLISION_LIST: u64 = 1 << 24;

/// The hard maximum for the per-deferred protector attribution:
/// 2^20 protectors.
pub const MAX_ACCOUNTING_PROTECTOR_LIST: u64 = 1 << 20;

/// The caller-controlled resource ceilings of the streaming
/// computation (the CI drives forced-small thresholds; the
/// production defaults bound the buffer, the run count, the
/// scratch bytes, and the MATERIALIZED RESULT LISTS). Every
/// field is validated against its hard documented maximum BEFORE
/// any allocation: a zero or over-maximum value refuses typed
/// (`accounting/limits`), and crossing a ceiling MID-COMPUTATION
/// refuses typed — never a truncated or weakened exact result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountingLimits {
    /// The in-memory entry buffer — the run spill threshold. A
    /// run is sorted and written whenever the buffer reaches
    /// this size. Range: 1..=[`MAX_ACCOUNTING_RUN_BUFFER_ENTRIES`].
    pub run_entry_buffer: usize,
    /// The maximum number of spill runs (the k-way merge's fan-in
    /// ceiling). Range: 1..=[`MAX_ACCOUNTING_RUN_COUNT`].
    pub max_runs: usize,
    /// The maximum total scratch bytes.
    /// Range: 1..=[`MAX_ACCOUNTING_SCRATCH_BYTE_CEILING`].
    pub max_scratch_bytes: u64,
    /// The maximum length of the materialized Mandatory list.
    /// Range: 1..=[`MAX_ACCOUNTING_CLASSIFIED_LIST`].
    pub max_mandatory_identities: u64,
    /// The maximum length of the materialized Deferred list.
    /// Range: 1..=[`MAX_ACCOUNTING_CLASSIFIED_LIST`].
    pub max_deferred_identities: u64,
    /// The maximum length of the materialized shared-reference
    /// list. Range: 1..=[`MAX_ACCOUNTING_SHARED_LIST`].
    pub max_shared_references: u64,
    /// The maximum length of the materialized namespace-collision
    /// list. Range: 1..=[`MAX_ACCOUNTING_COLLISION_LIST`].
    pub max_namespace_collisions: u64,
    /// The maximum protector-attribution length per deferred
    /// identity. Range: 1..=[`MAX_ACCOUNTING_PROTECTOR_LIST`].
    pub max_protectors_per_deferred: u64,
}

impl Default for AccountingLimits {
    fn default() -> Self {
        AccountingLimits {
            run_entry_buffer: 8192,
            max_runs: 4096,
            max_scratch_bytes: 1 << 32,
            max_mandatory_identities: MAX_ACCOUNTING_CLASSIFIED_LIST,
            max_deferred_identities: MAX_ACCOUNTING_CLASSIFIED_LIST,
            max_shared_references: MAX_ACCOUNTING_SHARED_LIST,
            max_namespace_collisions: MAX_ACCOUNTING_COLLISION_LIST,
            max_protectors_per_deferred: MAX_ACCOUNTING_PROTECTOR_LIST,
        }
    }
}

/// The complete, allocation-preceding validation: every field
/// within [1, its hard maximum] or the typed refusal.
fn validate_limits(limits: &AccountingLimits) -> Result<(), MediumError> {
    let valid = (1..=MAX_ACCOUNTING_RUN_BUFFER_ENTRIES).contains(&limits.run_entry_buffer)
        && (1..=MAX_ACCOUNTING_RUN_COUNT).contains(&limits.max_runs)
        && (1..=MAX_ACCOUNTING_SCRATCH_BYTE_CEILING).contains(&limits.max_scratch_bytes)
        && (1..=MAX_ACCOUNTING_CLASSIFIED_LIST).contains(&limits.max_mandatory_identities)
        && (1..=MAX_ACCOUNTING_CLASSIFIED_LIST).contains(&limits.max_deferred_identities)
        && (1..=MAX_ACCOUNTING_SHARED_LIST).contains(&limits.max_shared_references)
        && (1..=MAX_ACCOUNTING_COLLISION_LIST).contains(&limits.max_namespace_collisions)
        && (1..=MAX_ACCOUNTING_PROTECTOR_LIST).contains(&limits.max_protectors_per_deferred);
    if valid {
        Ok(())
    } else {
        Err(MediumError::Chain {
            rule: "accounting/limits",
        })
    }
}

// ============================== the identity entry codec (scratch)

/// The fixed scratch entry size: identity (32) + role (1) +
/// source kind (1) + source value (32).
const ACCOUNTING_ENTRY_BYTES: usize = 66;
/// The scratch run framing: the magic, the version, the count.
const ACCOUNTING_RUN_MAGIC: &[u8; 8] = b"S7A3RUN1";
const ACCOUNTING_RUN_VERSION: u8 = 1;
const ACCOUNTING_RUN_HEADER_BYTES: usize = 17;
/// The keyed transport trailer's width (SHA-256).
const ACCOUNTING_RUN_MAC_BYTES: usize = 32;
/// The survivor-reference spool's file name inside the workspace.
const SURVIVOR_SPOOL_FILE: &str = "survivor-refs";

const ROLE_RECORD: u8 = 0;
const ROLE_OBJECT: u8 = 1;

const SOURCE_CANDIDATE_UNIT: u8 = 0;
const SOURCE_PRIOR_BATCH_AUDIT: u8 = 1;
const SOURCE_PROTECTOR_ANCHOR: u8 = 2;
const SOURCE_PROTECTOR_FUTURE_MEMBER: u8 = 3;
const SOURCE_PROTECTOR_LIVE_CHAIN: u8 = 4;
const SOURCE_PROTECTOR_BOUNDARY: u8 = 5;
const SOURCE_PROTECTOR_BELOW_BOUNDARY: u8 = 6;

fn role_byte(role: StorageRole) -> u8 {
    match role {
        StorageRole::Record => ROLE_RECORD,
        StorageRole::Object => ROLE_OBJECT,
    }
}

fn protector_kind_byte(kind: ProtectorKind) -> u8 {
    match kind {
        ProtectorKind::RangeAnchor => SOURCE_PROTECTOR_ANCHOR,
        ProtectorKind::FutureBatchMember => SOURCE_PROTECTOR_FUTURE_MEMBER,
        ProtectorKind::LiveChainRevision => SOURCE_PROTECTOR_LIVE_CHAIN,
        ProtectorKind::RangeBoundary => SOURCE_PROTECTOR_BOUNDARY,
        ProtectorKind::BelowBoundaryHistory => SOURCE_PROTECTOR_BELOW_BOUNDARY,
    }
}

/// A numeric source value (a unit or batch index) in the fixed
/// 32-byte field: 8 big-endian bytes, zero-padded.
fn numeric_source(value: u64) -> [u8; 32] {
    let mut source = [0u8; 32];
    source[..8].copy_from_slice(&value.to_be_bytes());
    source
}

fn decode_numeric_source(source: &[u8; 32]) -> Result<u64, MediumError> {
    let bytes: [u8; 8] = source[..8].try_into().map_err(|_| MediumError::Chain {
        rule: "accounting/scratch-framing",
    })?;
    if source[8..] != [0u8; 24][..] {
        return Err(MediumError::Chain {
            rule: "accounting/scratch-framing",
        });
    }
    Ok(u64::from_be_bytes(bytes))
}

fn decode_protector_kind(kind: u8) -> Result<ProtectorKind, MediumError> {
    match kind {
        SOURCE_PROTECTOR_ANCHOR => Ok(ProtectorKind::RangeAnchor),
        SOURCE_PROTECTOR_FUTURE_MEMBER => Ok(ProtectorKind::FutureBatchMember),
        SOURCE_PROTECTOR_LIVE_CHAIN => Ok(ProtectorKind::LiveChainRevision),
        SOURCE_PROTECTOR_BOUNDARY => Ok(ProtectorKind::RangeBoundary),
        SOURCE_PROTECTOR_BELOW_BOUNDARY => Ok(ProtectorKind::BelowBoundaryHistory),
        _ => Err(MediumError::Chain {
            rule: "accounting/scratch-framing",
        }),
    }
}

/// One accounted identity stream entry. The derived order
/// (identity, role, kind, source) is the total deterministic key
/// the merge and the classification consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct AccountingEntry {
    identity: Hash,
    role: u8,
    source_kind: u8,
    source: [u8; 32],
}

fn encode_entry(entry: &AccountingEntry) -> [u8; ACCOUNTING_ENTRY_BYTES] {
    let mut bytes = [0u8; ACCOUNTING_ENTRY_BYTES];
    bytes[..32].copy_from_slice(entry.identity.as_bytes());
    bytes[32] = entry.role;
    bytes[33] = entry.source_kind;
    bytes[34..].copy_from_slice(&entry.source);
    bytes
}

fn decode_entry(bytes: &[u8; ACCOUNTING_ENTRY_BYTES]) -> Result<AccountingEntry, MediumError> {
    let mut identity_bytes = [0u8; 32];
    identity_bytes.copy_from_slice(&bytes[..32]);
    let identity = Hash::from_bytes(&identity_bytes).ok_or(MediumError::Chain {
        rule: "accounting/scratch-framing",
    })?;
    if identity == Hash::ZERO {
        return Err(MediumError::Chain {
            rule: "accounting/scratch-framing",
        });
    }
    if bytes[32] > ROLE_OBJECT || bytes[33] > SOURCE_PROTECTOR_BELOW_BOUNDARY {
        return Err(MediumError::Chain {
            rule: "accounting/scratch-framing",
        });
    }
    if (bytes[33] == SOURCE_CANDIDATE_UNIT || bytes[33] == SOURCE_PRIOR_BATCH_AUDIT)
        && bytes[34 + 8..] != [0u8; 24][..]
    {
        return Err(MediumError::Chain {
            rule: "accounting/scratch-framing",
        });
    }
    let mut source = [0u8; 32];
    source.copy_from_slice(&bytes[34..]);
    Ok(AccountingEntry {
        identity,
        role: bytes[32],
        source_kind: bytes[33],
        source,
    })
}

// ============================== the scratch transport integrity

/// The per-computation transport key: generated in-process from
/// the operating system's entropy (two independent OS-seeded
/// hasher states), never written to disk, and therefore not
/// forgeable by any file rewrite between the spill and the merge.
/// The MAC it keys is a TRANSPORT GUARD over the scratch bytes —
/// never a classification input.
#[derive(Clone, Copy)]
struct ScratchKey([u8; 16]);

fn fresh_scratch_key() -> ScratchKey {
    let mut key = [0u8; 16];
    let first = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let second = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    key[..8].copy_from_slice(&first.to_be_bytes());
    key[8..].copy_from_slice(&second.to_be_bytes());
    ScratchKey(key)
}

/// The stream discriminator for the survivor-reference spool —
/// distinct from every run index, so a spool record can never be
/// replayed as a run entry or vice versa.
const SPOOL_STREAM: u64 = u64::MAX;

/// The keyed transport MAC: SHA-256(key || stream || content).
/// For a run, the stream is the run's POSITION index (a reordered
/// or renamed run fails its position's recomputation); for the
/// spool, it is the dedicated discriminator.
fn transport_mac(key: &ScratchKey, stream: u64, content: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(key.0);
    hasher.update(stream.to_be_bytes());
    hasher.update(content);
    hasher.finalize().into()
}

// ================================== the external identity sorter

struct IdentitySorter {
    limits: AccountingLimits,
    key: ScratchKey,
    dir: std::path::PathBuf,
    buffer: Vec<AccountingEntry>,
    runs: usize,
    run_index: u64,
    entries_written: u64,
    bytes_written: u64,
}

impl IdentitySorter {
    fn new(limits: &AccountingLimits, scratch_root: &std::path::Path) -> Result<Self, MediumError> {
        // The allocation-preceding validation (defense-in-depth:
        // the public entry validated first; the sorter never
        // allocates or creates anything on a pathological input).
        validate_limits(limits)?;
        std::fs::create_dir_all(scratch_root).map_err(|_| MediumError::Chain {
            rule: "accounting/scratch-io",
        })?;
        // The EXCLUSIVELY-created working directory: create_dir
        // fails on any preexisting name, so a leftover directory
        // can never be reused (a bounded retry over fresh
        // time-based names; a persistent collision refuses typed).
        let mut dir = None;
        for _ in 0..4 {
            let unique = format!(
                "a3-accounting-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let candidate = scratch_root.join(unique);
            if std::fs::create_dir(&candidate).is_ok() {
                dir = Some(candidate);
                break;
            }
        }
        let Some(dir) = dir else {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-io",
            });
        };
        Ok(IdentitySorter {
            limits: *limits,
            key: fresh_scratch_key(),
            dir,
            buffer: Vec::with_capacity(limits.run_entry_buffer),
            runs: 0,
            run_index: 0,
            entries_written: 0,
            bytes_written: 0,
        })
    }

    fn push(&mut self, entry: AccountingEntry) -> Result<(), MediumError> {
        if entry.identity == Hash::ZERO {
            return Err(MediumError::Chain {
                rule: "accounting/zero-identity",
            });
        }
        self.buffer.push(entry);
        if self.buffer.len() >= self.limits.run_entry_buffer {
            self.spill()?;
        }
        Ok(())
    }

    fn push_candidate(
        &mut self,
        identity: Hash,
        role: StorageRole,
        unit: u64,
    ) -> Result<(), MediumError> {
        self.push(AccountingEntry {
            identity,
            role: role_byte(role),
            source_kind: SOURCE_CANDIDATE_UNIT,
            source: numeric_source(unit),
        })
    }

    fn push_prior(
        &mut self,
        identity: Hash,
        role: StorageRole,
        batch: u64,
    ) -> Result<(), MediumError> {
        self.push(AccountingEntry {
            identity,
            role: role_byte(role),
            source_kind: SOURCE_PRIOR_BATCH_AUDIT,
            source: numeric_source(batch),
        })
    }

    fn push_protector(
        &mut self,
        identity: Hash,
        role: StorageRole,
        kind: ProtectorKind,
        root: Hash,
    ) -> Result<(), MediumError> {
        let mut source = [0u8; 32];
        source.copy_from_slice(root.as_bytes());
        self.push(AccountingEntry {
            identity,
            role: role_byte(role),
            source_kind: protector_kind_byte(kind),
            source,
        })
    }

    fn spill(&mut self) -> Result<(), MediumError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        if self.runs >= self.limits.max_runs {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-run-limit",
            });
        }
        self.buffer.sort_unstable();
        // The CHECKED byte arithmetic: the entry payload and the
        // total run size are computed with checked operations
        // (the hard maxima keep these far from overflow; the
        // checks are the defense-in-depth that no pathological
        // input can wrap around).
        let entry_bytes = (self.buffer.len() as u64)
            .checked_mul(ACCOUNTING_ENTRY_BYTES as u64)
            .ok_or(MediumError::Chain {
                rule: "accounting/scratch-byte-limit",
            })?;
        let size = (ACCOUNTING_RUN_HEADER_BYTES as u64 + ACCOUNTING_RUN_MAC_BYTES as u64)
            .checked_add(entry_bytes)
            .ok_or(MediumError::Chain {
                rule: "accounting/scratch-byte-limit",
            })?;
        if self
            .bytes_written
            .checked_add(size)
            .is_none_or(|total| total > self.limits.max_scratch_bytes)
        {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-byte-limit",
            });
        }
        let path = self.run_path(self.run_index);
        let mut bytes = Vec::with_capacity(
            ACCOUNTING_RUN_HEADER_BYTES + self.buffer.len() * ACCOUNTING_ENTRY_BYTES,
        );
        bytes.extend_from_slice(ACCOUNTING_RUN_MAGIC);
        bytes.push(ACCOUNTING_RUN_VERSION);
        bytes.extend_from_slice(&(self.buffer.len() as u64).to_be_bytes());
        for entry in &self.buffer {
            bytes.extend_from_slice(&encode_entry(entry));
        }
        // The keyed transport trailer: the run's position-bound
        // MAC over exactly the bytes written above.
        bytes.extend_from_slice(&transport_mac(&self.key, self.run_index, &bytes));
        // The EXCLUSIVE creation: a preexisting file at the run
        // path (never written by this computation) refuses.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-io",
            })?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-io",
            })?;
        self.runs += 1;
        self.run_index += 1;
        self.entries_written += self.buffer.len() as u64;
        self.bytes_written += size;
        self.buffer.clear();
        Ok(())
    }

    fn run_path(&self, index: u64) -> std::path::PathBuf {
        self.dir.join(format!("run-{index:06}"))
    }

    fn finish(mut self) -> Result<MergedRuns, MediumError> {
        self.spill()?;
        // The WORKSPACE validation: the directory contains exactly
        // the spool file and the expected run files — any
        // unexpected entry (a planted file, a renamed run, a
        // leftover) refuses typed before any run is consumed.
        let mut expected: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::from([SURVIVOR_SPOOL_FILE.to_owned()]);
        for index in 0..self.runs as u64 {
            expected.insert(format!("run-{index:06}"));
        }
        let mut observed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&self.dir).map_err(|_| MediumError::Chain {
            rule: "accounting/scratch-io",
        })? {
            let entry = entry.map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-io",
            })?;
            observed.insert(entry.file_name().to_string_lossy().into_owned());
        }
        if observed != expected {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-integrity",
            });
        }
        let mut readers = Vec::with_capacity(self.runs);
        for index in 0..self.runs as u64 {
            readers.push(RunReader::open(&self.run_path(index), &self.key, index)?);
        }
        let mut heap = std::collections::BinaryHeap::new();
        for (index, reader) in readers.iter().enumerate() {
            if let Some(current) = reader.current {
                heap.push(std::cmp::Reverse((current, index)));
            }
        }
        let facts = ScratchReport {
            runs_written: self.runs as u64,
            bytes_written: self.bytes_written,
            entries_written: self.entries_written,
            cleanup_residual: false,
        };
        // The directory hands off to the merge: taking it out of
        // self neutralizes the sorter's drop-cleanup so the runs
        // stay readable (every ERROR path above leaves the
        // directory in self, whose Drop removes it).
        let dir = std::mem::take(&mut self.dir);
        Ok(MergedRuns {
            readers,
            heap,
            dir,
            facts,
        })
    }
}

impl Drop for IdentitySorter {
    fn drop(&mut self) {
        // The best-effort cleanup on EVERY failing path: a sorter
        // that never reached a successful finish removes its
        // exclusively-created workspace (a failure here is
        // silent — no success claim exists on an error path).
        if !self.dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// One spilled run's streaming reader: every entry's RAW WIRE
/// BYTES feed the incremental transport MAC, the entries are
/// validated as STRICTLY SORTED within the run (the merge's
/// correctness invariant), and the consumption completes only
/// when the trailing MAC equals the recomputation for THIS run's
/// position — a run rewritten between the spill and the merge
/// (even with valid framing and counts), a reordered or renamed
/// run file, or any truncated or extended form refuses typed.
struct RunReader {
    reader: std::io::BufReader<std::fs::File>,
    mac: Sha256,
    remaining: u64,
    current: Option<AccountingEntry>,
    previous: Option<AccountingEntry>,
    finished: bool,
}

impl RunReader {
    fn open(path: &std::path::Path, key: &ScratchKey, stream: u64) -> Result<Self, MediumError> {
        let mut file = std::fs::File::open(path).map_err(|_| MediumError::Chain {
            rule: "accounting/scratch-io",
        })?;
        let mut header = [0u8; ACCOUNTING_RUN_HEADER_BYTES];
        file.read_exact(&mut header)
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-framing",
            })?;
        if &header[..8] != ACCOUNTING_RUN_MAGIC.as_slice() || header[8] != ACCOUNTING_RUN_VERSION {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-framing",
            });
        }
        let count = u64::from_be_bytes(header[9..].try_into().map_err(|_| MediumError::Chain {
            rule: "accounting/scratch-framing",
        })?);
        let mut mac = Sha256::new();
        mac.update(key.0);
        mac.update(stream.to_be_bytes());
        mac.update(header);
        let mut run = RunReader {
            reader: std::io::BufReader::new(file),
            mac,
            remaining: count,
            current: None,
            previous: None,
            finished: false,
        };
        if count > 0 {
            run.step()?;
        } else {
            run.finish_run()?;
        }
        Ok(run)
    }

    fn step(&mut self) -> Result<(), MediumError> {
        let mut bytes = [0u8; ACCOUNTING_ENTRY_BYTES];
        self.reader
            .read_exact(&mut bytes)
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-framing",
            })?;
        // The raw wire bytes feed the transport MAC — ANY byte
        // change, however syntactically valid, breaks the trailer.
        self.mac.update(bytes);
        let entry = decode_entry(&bytes)?;
        // The sorted-order validation: the merge requires each
        // run's entries in strictly nondecreasing total order.
        if let Some(previous) = &self.previous
            && entry < *previous
        {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-order",
            });
        }
        self.previous = Some(entry);
        self.current = Some(entry);
        self.remaining -= 1;
        Ok(())
    }

    /// The run's completion: the trailing MAC is read and compared
    /// with the incremental recomputation — the accumulator has
    /// fed exactly the writer's MAC input (the key, this run's
    /// position, the header, and every entry's raw wire bytes) —
    /// and the file must end exactly there.
    fn finish_run(&mut self) -> Result<(), MediumError> {
        let mut trailer = [0u8; ACCOUNTING_RUN_MAC_BYTES];
        self.reader
            .read_exact(&mut trailer)
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-framing",
            })?;
        let recomputed: [u8; 32] = self.mac.clone().finalize().into();
        if trailer != recomputed {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-integrity",
            });
        }
        let mut probe = [0u8; 1];
        match self.reader.read(&mut probe) {
            Ok(0) => {}
            Ok(_) => {
                return Err(MediumError::Chain {
                    rule: "accounting/scratch-framing",
                });
            }
            Err(_) => {
                return Err(MediumError::Chain {
                    rule: "accounting/scratch-io",
                });
            }
        }
        self.finished = true;
        self.current = None;
        Ok(())
    }

    fn advance(&mut self) -> Result<(), MediumError> {
        if self.remaining > 0 {
            self.step()?;
        } else if !self.finished {
            self.finish_run()?;
        } else {
            self.current = None;
        }
        Ok(())
    }
}

/// The deterministic k-way merge over the spilled runs — the
/// single sorted, exact-duplicate-free identity stream the
/// classification consumes. The scratch directory is removed on
/// [`MergedRuns::cleanup`] (the success path, with the residual
/// recorded) and best-effort on drop (the error paths — no
/// success claim exists there).
struct MergedRuns {
    readers: Vec<RunReader>,
    heap: std::collections::BinaryHeap<std::cmp::Reverse<(AccountingEntry, usize)>>,
    dir: std::path::PathBuf,
    facts: ScratchReport,
}

impl MergedRuns {
    fn next(&mut self) -> Result<Option<AccountingEntry>, MediumError> {
        let Some(std::cmp::Reverse((entry, index))) = self.heap.pop() else {
            return Ok(None);
        };
        self.advance_run(index)?;
        while let Some(std::cmp::Reverse((top, _))) = self.heap.peek()
            && *top == entry
        {
            let Some(std::cmp::Reverse((_, duplicate))) = self.heap.pop() else {
                break;
            };
            self.advance_run(duplicate)?;
        }
        Ok(Some(entry))
    }

    fn advance_run(&mut self, index: usize) -> Result<(), MediumError> {
        self.readers[index].advance()?;
        if let Some(current) = self.readers[index].current {
            self.heap.push(std::cmp::Reverse((current, index)));
        }
        Ok(())
    }

    fn facts(&self) -> ScratchReport {
        self.facts
    }

    fn cleanup(mut self) -> bool {
        self.readers.clear();
        std::fs::remove_dir_all(&self.dir).is_ok()
    }
}

impl Drop for MergedRuns {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ================================ the survivor-reference spool

/// The per-record spool framing: [root: 32][kind: 1][count: 8]
/// followed by the count reference identities (32 bytes each).
const SPOOL_RECORD_HEADER_BYTES: usize = 41;
/// The sanity ceiling on one spooled record's reference count
/// (the records were written by this computation from canonical
/// record reference sets; a tampered count shifts the framing and
/// fails the transport MAC regardless).
const MAX_SPOOL_RECORD_REFERENCES: u64 = 1 << 24;

/// The walk-emitted survivor references' bounded scratch spool:
/// the anchors' live reference sets and the future members'
/// ORIGINAL preimage reference sets stream here record-by-record
/// during the walk (NEVER an in-memory aggregate), and the
/// post-walk pass reads them back one record at a time — one
/// survivor's reference working set resident at most — performing
/// the hash-verified read of every reference and the composite
/// closure verification. The spool carries the same keyed
/// transport MAC as the runs (under the dedicated stream
/// discriminator), is created exclusively, and lives inside the
/// sorter's workspace (removed with it on every path).
struct SurvivorRefSpool {
    path: std::path::PathBuf,
    key: ScratchKey,
    writer: std::io::BufWriter<std::fs::File>,
    mac: Sha256,
}

impl SurvivorRefSpool {
    fn create(sorter: &IdentitySorter) -> Result<Self, MediumError> {
        let path = sorter.dir.join(SURVIVOR_SPOOL_FILE);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-io",
            })?;
        let mut mac = Sha256::new();
        mac.update(sorter.key.0);
        mac.update(SPOOL_STREAM.to_be_bytes());
        Ok(SurvivorRefSpool {
            path,
            key: sorter.key,
            writer: std::io::BufWriter::new(file),
            mac,
        })
    }

    /// Appends one survivor root's reference record.
    fn append(
        &mut self,
        root: Hash,
        kind: ProtectorKind,
        references: &[Hash],
    ) -> Result<(), MediumError> {
        let mut record =
            Vec::with_capacity(SPOOL_RECORD_HEADER_BYTES + std::mem::size_of_val(references));
        record.extend_from_slice(root.as_bytes());
        record.push(protector_kind_byte(kind));
        record.extend_from_slice(&(references.len() as u64).to_be_bytes());
        for reference in references {
            record.extend_from_slice(reference.as_bytes());
        }
        self.writer
            .write_all(&record)
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-io",
            })?;
        self.mac.update(&record);
        Ok(())
    }

    /// Closes the writer with the transport trailer and returns
    /// the streaming reader over the completed file.
    fn into_reader(mut self) -> Result<SurvivorRefReader, MediumError> {
        let trailer: [u8; 32] = self.mac.clone().finalize().into();
        self.writer
            .write_all(&trailer)
            .and_then(|_| self.writer.flush())
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-io",
            })?;
        drop(self.writer);
        SurvivorRefReader::open(&self.path, self.key)
    }
}

/// The spool's streaming reader: yields one reference at a time
/// with its root, protector kind, and whether it is the record's
/// FIRST reference (the composite-closure position), validating
/// the framing and the transport trailer at the end.
struct SurvivorRefReader {
    reader: std::io::BufReader<std::fs::File>,
    mac: Sha256,
    root: Hash,
    kind: ProtectorKind,
    remaining: u64,
    first: bool,
    finished: bool,
}

impl SurvivorRefReader {
    fn open(spool_path: &std::path::Path, key: ScratchKey) -> Result<Self, MediumError> {
        let file = std::fs::File::open(spool_path).map_err(|_| MediumError::Chain {
            rule: "accounting/scratch-io",
        })?;
        let mut mac = Sha256::new();
        mac.update(key.0);
        mac.update(SPOOL_STREAM.to_be_bytes());
        Ok(SurvivorRefReader {
            reader: std::io::BufReader::new(file),
            mac,
            root: Hash::ZERO,
            kind: ProtectorKind::RangeAnchor,
            remaining: 0,
            first: false,
            finished: false,
        })
    }

    fn next_reference(&mut self) -> Result<Option<(Hash, ProtectorKind, Hash, bool)>, MediumError> {
        if self.remaining == 0 && !self.finished {
            // The next record header, or the transport trailer at
            // the true end. The probe ACCUMULATES byte-by-byte so
            // the end detection never consumes the trailer it must
            // then read: a full 41-byte fill is a record header,
            // an exact 32-byte fill at the end IS the trailer, and
            // any other termination refuses the framing.
            let mut header = [0u8; SPOOL_RECORD_HEADER_BYTES];
            let mut filled = 0usize;
            while filled < SPOOL_RECORD_HEADER_BYTES {
                let read =
                    self.reader
                        .read(&mut header[filled..])
                        .map_err(|_| MediumError::Chain {
                            rule: "accounting/scratch-io",
                        })?;
                if read == 0 {
                    break;
                }
                filled += read;
            }
            if filled == SPOOL_RECORD_HEADER_BYTES {
                self.mac.update(header);
                let mut root_bytes = [0u8; 32];
                root_bytes.copy_from_slice(&header[..32]);
                let root = Hash::from_bytes(&root_bytes).ok_or(MediumError::Chain {
                    rule: "accounting/scratch-framing",
                })?;
                if root == Hash::ZERO {
                    return Err(MediumError::Chain {
                        rule: "accounting/scratch-framing",
                    });
                }
                let kind = decode_protector_kind(header[32])?;
                let count = u64::from_be_bytes(header[33..].try_into().map_err(|_| {
                    MediumError::Chain {
                        rule: "accounting/scratch-framing",
                    }
                })?);
                if count == 0 || count > MAX_SPOOL_RECORD_REFERENCES {
                    return Err(MediumError::Chain {
                        rule: "accounting/scratch-framing",
                    });
                }
                self.root = root;
                self.kind = kind;
                self.remaining = count;
                self.first = true;
            } else if filled == ACCOUNTING_RUN_MAC_BYTES {
                // The accumulated bytes ARE the transport trailer.
                let trailer: [u8; ACCOUNTING_RUN_MAC_BYTES] = header[..ACCOUNTING_RUN_MAC_BYTES]
                    .try_into()
                    .expect("the trailer width is the accumulated length");
                let recomputed: [u8; 32] = self.mac.clone().finalize().into();
                if trailer != recomputed {
                    return Err(MediumError::Chain {
                        rule: "accounting/scratch-integrity",
                    });
                }
                let mut probe = [0u8; 1];
                match self.reader.read(&mut probe) {
                    Ok(0) => {}
                    Ok(_) => {
                        return Err(MediumError::Chain {
                            rule: "accounting/scratch-framing",
                        });
                    }
                    Err(_) => {
                        return Err(MediumError::Chain {
                            rule: "accounting/scratch-io",
                        });
                    }
                }
                self.finished = true;
                return Ok(None);
            } else {
                return Err(MediumError::Chain {
                    rule: "accounting/scratch-framing",
                });
            }
        }
        if self.finished {
            return Ok(None);
        }
        let mut reference_bytes = [0u8; 32];
        self.reader
            .read_exact(&mut reference_bytes)
            .map_err(|_| MediumError::Chain {
                rule: "accounting/scratch-framing",
            })?;
        self.mac.update(reference_bytes);
        let reference = Hash::from_bytes(&reference_bytes).ok_or(MediumError::Chain {
            rule: "accounting/scratch-framing",
        })?;
        if reference == Hash::ZERO {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-framing",
            });
        }
        let first = self.first;
        self.first = false;
        self.remaining -= 1;
        Ok(Some((self.root, self.kind, reference, first)))
    }
}

// ============================================ the classification pass

#[derive(Default)]
struct IdentityGroup {
    candidate_units: std::collections::BTreeSet<u64>,
    protectors: std::collections::BTreeSet<ProtectorRef>,
    prior_batches: std::collections::BTreeSet<u64>,
}

#[derive(Default)]
struct ClassificationOutputs {
    mandatory: Vec<MandatoryIdentity>,
    deferred: Vec<DeferredIdentity>,
    shared: Vec<SharedReference>,
    collisions: Vec<NamespaceCollision>,
}

fn classify_merged(
    merged: &mut MergedRuns,
    limits: &AccountingLimits,
) -> Result<ClassificationOutputs, MediumError> {
    let mut outputs = ClassificationOutputs::default();
    let mut groups = [IdentityGroup::default(), IdentityGroup::default()];
    let mut current: Option<Hash> = None;
    while let Some(entry) = merged.next()? {
        if current != Some(entry.identity) {
            if let Some(identity) = current.take() {
                classify_identity(identity, &groups, &mut outputs, limits)?;
            }
            groups = [IdentityGroup::default(), IdentityGroup::default()];
            current = Some(entry.identity);
        }
        let role = entry.role as usize;
        if role > 1 {
            return Err(MediumError::Chain {
                rule: "accounting/scratch-framing",
            });
        }
        let group = &mut groups[role];
        match entry.source_kind {
            SOURCE_CANDIDATE_UNIT => {
                group
                    .candidate_units
                    .insert(decode_numeric_source(&entry.source)?);
            }
            SOURCE_PRIOR_BATCH_AUDIT => {
                group
                    .prior_batches
                    .insert(decode_numeric_source(&entry.source)?);
            }
            SOURCE_PROTECTOR_ANCHOR..=SOURCE_PROTECTOR_BELOW_BOUNDARY => {
                let kind = decode_protector_kind(entry.source_kind)?;
                let mut root_bytes = [0u8; 32];
                root_bytes.copy_from_slice(&entry.source);
                let root = Hash::from_bytes(&root_bytes).ok_or(MediumError::Chain {
                    rule: "accounting/scratch-framing",
                })?;
                group.protectors.insert(ProtectorRef { kind, root });
            }
            _ => {
                return Err(MediumError::Chain {
                    rule: "accounting/scratch-framing",
                });
            }
        }
    }
    if let Some(identity) = current.take() {
        classify_identity(identity, &groups, &mut outputs, limits)?;
    }
    Ok(outputs)
}

fn classify_identity(
    identity: Hash,
    groups: &[IdentityGroup; 2],
    outputs: &mut ClassificationOutputs,
    limits: &AccountingLimits,
) -> Result<(), MediumError> {
    for (role_index, role) in [(0usize, StorageRole::Record), (1, StorageRole::Object)] {
        let group = &groups[role_index];
        if group.candidate_units.is_empty() {
            continue;
        }
        let other = &groups[1 - role_index];
        let first_seen_unit = *group
            .candidate_units
            .iter()
            .next()
            .expect("the candidate-unit set is non-empty, so it has a first element");
        // The authenticated exclusion's invariant (the accepted
        // partition's own rule): a retiring RECORD identity that a
        // surviving root also holds as a RECORD is an internal
        // inconsistency — refuse, never silently override.
        if role == StorageRole::Record && !group.protectors.is_empty() {
            return Err(MediumError::Chain {
                rule: "accounting/record-survivor-overlap",
            });
        }
        if !group.protectors.is_empty() || !other.protectors.is_empty() {
            let mut protectors: Vec<ProtectorRef> = group.protectors.iter().copied().collect();
            for protector in &other.protectors {
                if !group.protectors.contains(protector) {
                    protectors.push(*protector);
                }
            }
            protectors.sort_unstable();
            // THE MATERIALIZED-OUTPUT CEILINGS: crossing one
            // refuses typed — never a truncated or weakened exact
            // result (the defaults sit above the ledger-bounded
            // universe, so only a caller's deliberately small
            // ceiling or a pathological state reaches them).
            if protectors.len() as u64 > limits.max_protectors_per_deferred {
                return Err(MediumError::Chain {
                    rule: "accounting/protector-limit",
                });
            }
            if outputs.deferred.len() as u64 >= limits.max_deferred_identities {
                return Err(MediumError::Chain {
                    rule: "accounting/deferred-limit",
                });
            }
            let collision = !other.protectors.is_empty();
            outputs.deferred.push(DeferredIdentity {
                identity,
                role,
                first_seen_unit,
                namespace_collision: collision,
                protectors,
            });
            if collision {
                if outputs.collisions.len() as u64 >= limits.max_namespace_collisions {
                    return Err(MediumError::Chain {
                        rule: "accounting/collision-limit",
                    });
                }
                outputs.collisions.push(NamespaceCollision {
                    identity,
                    candidate_role: role,
                    first_seen_unit,
                    protectors: other.protectors.iter().copied().collect(),
                    classification: CollisionClassification::ConservativelyProtected,
                });
            }
        } else {
            let conditional = !(group.prior_batches.is_empty() && other.prior_batches.is_empty());
            if outputs.mandatory.len() as u64 >= limits.max_mandatory_identities {
                return Err(MediumError::Chain {
                    rule: "accounting/mandatory-limit",
                });
            }
            outputs.mandatory.push(MandatoryIdentity {
                identity,
                role,
                first_seen_unit,
                conditional_on_prior_batches: conditional,
            });
        }
        if group.candidate_units.len() >= 2 {
            if outputs.shared.len() as u64 >= limits.max_shared_references {
                return Err(MediumError::Chain {
                    rule: "accounting/shared-limit",
                });
            }
            outputs.shared.push(SharedReference {
                identity,
                role,
                referencing_units: group.candidate_units.iter().copied().collect(),
            });
        }
    }
    Ok(())
}

// ============================== the survivor-closure streaming helper

fn push_survivor_closure(
    sorter: &mut IdentitySorter,
    reads: &mut MediumReads<'_>,
    root: Hash,
    kind: ProtectorKind,
    refs: &[Hash],
    survivor_entries: &mut u64,
    composite_closure_roots: &mut u64,
) -> Result<(), MediumError> {
    // The accepted closure verification applied to the
    // AUTHENTICATED reference set (never a re-read of a
    // possibly-absent member record): the walk's steps carry the
    // anchors' live references and the future members' ORIGINAL
    // preimage references, and the accounting's own descents carry
    // the records they just read. Every reference is read through
    // the hash-verifying read; a composite /2 root's manifest is
    // decoded and every chunk envelope-verified — the same
    // machinery the audit collection uses. Missing or
    // unverifiable closure evidence refuses closed (the typed read
    // errors propagate with the failing identity). The canonical
    // reference sets are deduplicated by the record codec's own
    // construction.
    for (position, reference) in refs.iter().enumerate() {
        let bytes = reads.read_object_bytes(reference)?;
        sorter.push_protector(*reference, StorageRole::Object, kind, root)?;
        *survivor_entries += 1;
        if position == 0 {
            let object = StoredObject::new(bytes)?;
            if object.domain() == crate::composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN {
                let manifest = crate::composite_manifest::decode(object.bytes())
                    .map_err(MediumError::Format)?;
                for entry in &manifest.entries {
                    let chunk_bytes = reads.read_object_bytes(&entry.identity)?;
                    crate::composite_append::verify_chunk_dependency(
                        &chunk_bytes,
                        &entry.identity,
                        &manifest,
                    )
                    .map_err(|_error| MediumError::Chain {
                        rule: "accounting/closure-verification",
                    })?;
                    sorter.push_protector(entry.identity, StorageRole::Object, kind, root)?;
                    *survivor_entries += 1;
                }
                *composite_closure_roots += 1;
            }
        }
    }
    Ok(())
}

// ================================================ the accounting entry

/// (V2X-CP3-A3) THE EXACT PRESERVATION AND RETIREMENT-PARTITION
/// ACCOUNTING — the read-only, bounded, streaming evaluation of
/// one plan-defined batch of one authenticated checkpoint.
///
/// The prerequisites are established FIRST and completely (the
/// CP2 begin authority, the checkpoint metadata, the plan, the
/// audit-locator discovery, the selected audit's binding and
/// ordered-membership verification); the candidate universe then
/// streams from the immutable audit (never from live retirement
/// records); the survivor universe streams from the A2 walk, the
/// live HEAD-to-MK chain, and the below-boundary history; the
/// prior-batch references stream from the prior batches' own
/// audits; and the deterministic external sort joins them into
/// the exact Mandatory/Deferred partition with the shared,
/// collision, and conditional classifications. See the module
/// documentation for the full contract.
///
/// The result NEVER authorizes deletion: it carries no
/// `safe_to_delete` claim, no permit, and no durable certificate —
/// the separately authorized destructive integration must
/// re-establish the provider coordination boundary its own way.
pub fn account_batch_retirement(
    medium: &mut dyn CanonicalMedium,
    mk: &Hash,
    batch_index: u64,
    limits: &AccountingLimits,
    scratch_root: &std::path::Path,
) -> Result<RetirementAccounting, MediumError> {
    // (0) THE ALLOCATION-PRECEDING LIMIT VALIDATION — every field
    // within [1, its hard documented maximum] before any buffer
    // or scratch file exists.
    validate_limits(limits)?;
    // (1) The live HEAD.
    let head = medium.head()?.ok_or(MediumError::Chain {
        rule: "accounting/unfounded",
    })?;
    // (2) THE CP2 PLAN-BOUND BEGIN AUTHORITY — the freshness
    // discipline: the medium's own coordination-scoped
    // verification of the checkpoint, the plan, the complete
    // below-begin audit set, and the four-field begin binding. An
    // absent or degraded begin refuses typed here.
    let begin = medium.verify_begin_authority_v2(mk)?;
    let begin_revision = begin.id();
    // (3) THE CHECKPOINT MK AND ITS PERMANENT METADATA — the
    // canonical descent match, the SK binding, and the non-empty
    // retirement index (the readiness reader's exact checks).
    let metadata = {
        let reads = &mut MediumReads::new(medium);
        let Some((_mk_id, meta_bytes)) =
            crate::reclaim::find_on_chain_via(reads, head, None, Some(mk))?
        else {
            return Err(MediumError::Chain {
                rule: "accounting/mk-not-installed",
            });
        };
        let metadata = crate::checkpoint::CheckpointMetadata::decode(&meta_bytes)
            .map_err(MediumError::Format)?;
        let mk_record = crate::reclaim::read_record_via(reads, mk)?;
        if mk_record.content_refs.len() != 1 || Some(metadata.sk) != mk_record.parent {
            return Err(MediumError::Chain {
                rule: "accounting/metadata-binding",
            });
        }
        if metadata.index.is_empty() {
            return Err(MediumError::Chain {
                rule: "accounting/empty-index",
            });
        }
        metadata
    };
    // (4) THE PLAN — the deterministic MK-bound key discovery and
    // the checkpoint binding (the MK, the SK, the pre-verified
    // range digest, the founding-derived seed) plus the canonical
    // ledger-byte ceiling.
    let (plan_revision, plan) = {
        let reads = &mut MediumReads::new(medium);
        let Some((plan_record, plan)) = find_installed_plan_via(reads, head, mk)? else {
            return Err(MediumError::Chain {
                rule: "accounting/plan-absent",
            });
        };
        (plan_record, plan)
    };
    if plan.mk != *mk
        || plan.sk != metadata.sk
        || plan.range_digest != metadata.range_digest
        || plan.seed != metadata.seed
    {
        return Err(MediumError::Chain {
            rule: "accounting/plan-binding",
        });
    }
    if plan.ledger_byte_limit > MAX_RECLAMATION_LEDGER_BYTES {
        return Err(MediumError::Chain {
            rule: "accounting/ledger-byte-limit",
        });
    }
    // (5) THE SELECTED BATCH.
    let Some(selected) = plan.batches.iter().find(|batch| batch.index == batch_index) else {
        return Err(MediumError::Chain {
            rule: "accounting/unknown-batch",
        });
    };
    // (6) THE AUDIT LOCATORS — ONE single-descent multi-key
    // discovery for every plan-listed batch audit (O(chain)
    // once, never per batch), with the completeness refusal.
    let audit_locators = {
        let reads = &mut MediumReads::new(medium);
        let mut expected: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
        for batch in &plan.batches {
            expected.insert(reclamation_ledger_v2_idempotency_key(mk, batch.index));
        }
        let discovered = discover_keyed_set_via(reads, head, &expected)?;
        for batch in &plan.batches {
            if !discovered.contains_key(&reclamation_ledger_v2_idempotency_key(mk, batch.index)) {
                return Err(MediumError::Chain {
                    rule: "accounting/audit-absent",
                });
            }
        }
        discovered
    };
    // The per-audit loader: the record re-read (the shape, the
    // deterministic key, the single content reference), the
    // object's hash-verified read, the strict decode, the field
    // agreement, and the byte limit — one audit resident at most.
    let load_audit = |medium: &mut dyn CanonicalMedium, batch: u64| {
        let key = reclamation_ledger_v2_idempotency_key(mk, batch);
        let Some(&(record, object)) = audit_locators.get(&key) else {
            return Err(MediumError::Chain {
                rule: "accounting/audit-absent",
            });
        };
        let reads = &mut MediumReads::new(medium);
        let audit_record = crate::reclaim::read_record_via(reads, &record)?;
        if !matches!(audit_record.kind, crate::record::RecordKind::Regular)
            || audit_record.content_refs.len() != 1
            || audit_record.idempotency_key.as_deref() != Some(key.as_slice())
            || audit_record.content_refs[0] != object
        {
            return Err(MediumError::Chain {
                rule: "accounting/audit-shape",
            });
        }
        let bytes = reads.read_object_bytes(&object)?;
        let ledger = ReclamationLedgerV2::decode(&bytes).map_err(MediumError::Format)?;
        if ledger.mk != *mk
            || ledger.sk != plan.sk
            || ledger.batch != batch
            || ledger.range_digest != plan.range_digest
            || ledger.seed != plan.seed
        {
            return Err(MediumError::Chain {
                rule: "accounting/audit-binding",
            });
        }
        if ledger.canonical_bytes().len() as u64 > plan.ledger_byte_limit {
            return Err(MediumError::Chain {
                rule: "accounting/audit-byte-limit",
            });
        }
        Ok((record, object, ledger))
    };
    // (7) THE SELECTED AUDIT — the ordered unit sequence equals
    // the plan's batch membership exactly (position by position).
    let (audit_revision, audit_object, ledger) = load_audit(medium, batch_index)?;
    let unit_records: Vec<Hash> = ledger.units.iter().map(|unit| unit.record).collect();
    if unit_records != selected.records {
        return Err(MediumError::Chain {
            rule: "accounting/audit-order",
        });
    }
    let audit_units = ledger.units.len() as u64;
    // (8) THE SCRATCH SORTER AND THE CANDIDATE STREAM — the audit
    // is the authority; no live retirement record is read.
    let mut sorter = IdentitySorter::new(limits, scratch_root)?;
    for (unit_index, unit) in ledger.units.iter().enumerate() {
        sorter.push_candidate(unit.record, StorageRole::Record, unit_index as u64)?;
        for object in &unit.objects {
            sorter.push_candidate(*object, StorageRole::Object, unit_index as u64)?;
        }
    }
    drop(ledger);
    // (9) THE A2 AUTHENTICATED HISTORICAL WALK — the range's
    // original parent-by-parent history. The retained anchors and
    // the FUTURE-batch members are survivor roots (their record
    // identities stream immediately); the CURRENT-batch members
    // are collected for the walk/audit membership cross-check; the
    // PRIOR-batch members are neither (the prior audits carry
    // their references).
    let mut future_ids: Vec<Hash> = Vec::new();
    let mut anchor_ids: Vec<Hash> = Vec::new();
    let mut current_ids: Vec<Hash> = Vec::new();
    let mut boundary: Option<(Hash, bool)> = None;
    let mut survivor_entries: u64 = 0;
    // THE SURVIVOR-REFERENCE SPOOL: the walk-emitted reference
    // sets stream here record-by-record (NEVER an in-memory
    // aggregate); the post-walk pass reads them back one survivor
    // at a time for the hash-verified reads and the composite
    // closure verification.
    let mut spool = SurvivorRefSpool::create(&sorter)?;
    let walk_summary = crate::ancestry_history::walk_authenticated_history(
        medium,
        mk,
        batch_index,
        &mut |step| {
            match step {
                crate::ancestry_history::HistoricalStep::Member {
                    identity,
                    content_refs,
                    position,
                    ..
                } => match position {
                    crate::ancestry_history::BatchPosition::FutureBatch => {
                        future_ids.push(*identity);
                        spool.append(*identity, ProtectorKind::FutureBatchMember, content_refs)?;
                        sorter.push_protector(
                            *identity,
                            StorageRole::Record,
                            ProtectorKind::FutureBatchMember,
                            *identity,
                        )?;
                        survivor_entries += 1;
                    }
                    crate::ancestry_history::BatchPosition::CurrentBatch => {
                        current_ids.push(*identity);
                    }
                    crate::ancestry_history::BatchPosition::PriorBatch => {}
                },
                crate::ancestry_history::HistoricalStep::Anchor {
                    identity,
                    content_refs,
                    ..
                } => {
                    anchor_ids.push(*identity);
                    spool.append(*identity, ProtectorKind::RangeAnchor, content_refs)?;
                    sorter.push_protector(
                        *identity,
                        StorageRole::Record,
                        ProtectorKind::RangeAnchor,
                        *identity,
                    )?;
                    survivor_entries += 1;
                }
                crate::ancestry_history::HistoricalStep::Boundary {
                    identity,
                    prior_checkpoint,
                } => {
                    boundary = Some((*identity, *prior_checkpoint));
                    sorter.push_protector(
                        *identity,
                        StorageRole::Record,
                        ProtectorKind::RangeBoundary,
                        *identity,
                    )?;
                    survivor_entries += 1;
                }
            }
            Ok(())
        },
    )?;
    // The walk/audit membership cross-check: the walk's current
    // members (the canonical order) equal the audit's ordered unit
    // records exactly — a contradiction refuses typed.
    if current_ids != unit_records {
        return Err(MediumError::Chain {
            rule: "accounting/walk-audit-membership",
        });
    }
    let (boundary_id, boundary_prior_checkpoint) =
        boundary.expect("the completed walk emitted the boundary");
    // (10) THE SURVIVOR CLOSURES, THE BELOW-BOUNDARY HISTORY, AND
    // THE LIVE CHAIN — the reader-generic streaming over the
    // accepted closure verification.
    let mut composite_closure_roots: u64 = 0;
    let below_boundary_revisions;
    let live_chain_revisions;
    {
        let reads = &mut MediumReads::new(medium);
        // The retained anchors' and future members' references —
        // streamed back from the spool ONE SURVIVOR AT A TIME
        // (never an aggregate), every reference read through the
        // hash-verifying read, the composite closure verified on
        // each record's first reference.
        let mut spool_reader = spool.into_reader()?;
        while let Some((root, kind, reference, first)) = spool_reader.next_reference()? {
            let bytes = reads.read_object_bytes(&reference)?;
            sorter.push_protector(reference, StorageRole::Object, kind, root)?;
            survivor_entries += 1;
            if first {
                let object = StoredObject::new(bytes)?;
                if object.domain() == crate::composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN {
                    composite_closure_roots += 1;
                    let manifest = crate::composite_manifest::decode(object.bytes())
                        .map_err(MediumError::Format)?;
                    for entry in &manifest.entries {
                        let chunk_bytes = reads.read_object_bytes(&entry.identity)?;
                        crate::composite_append::verify_chunk_dependency(
                            &chunk_bytes,
                            &entry.identity,
                            &manifest,
                        )
                        .map_err(|_error| MediumError::Chain {
                            rule: "accounting/closure-verification",
                        })?;
                        sorter.push_protector(entry.identity, StorageRole::Object, kind, root)?;
                        survivor_entries += 1;
                    }
                }
            }
        }
        drop(spool_reader);
        // The boundary's own references (the prior MK's metadata
        // object, or the founding's content).
        let boundary_record = crate::reclaim::read_record_via(reads, &boundary_id)?;
        push_survivor_closure(
            &mut sorter,
            reads,
            boundary_id,
            ProtectorKind::RangeBoundary,
            &boundary_record.content_refs,
            &mut survivor_entries,
            &mut composite_closure_roots,
        )?;
        // The below-boundary history: ONLY when the boundary is a
        // prior checkpoint MK — every record from the boundary's
        // parent down to the campaign founding, walked live with
        // hash-verifying reads, NO skip tables, every record
        // protected. An absent record refuses typed (the region is
        // never silently crossed or treated as empty); the
        // terminal must be the genuine founding shape.
        below_boundary_revisions = if boundary_prior_checkpoint {
            let Some(mut cursor) = boundary_record.parent else {
                return Err(MediumError::Chain {
                    rule: "accounting/below-boundary-shape",
                });
            };
            let mut seen: std::collections::BTreeSet<Hash> = std::collections::BTreeSet::new();
            let mut revisions = 0u64;
            let mut guard = 0u64;
            loop {
                guard += 1;
                if guard > ACCOUNTING_DESCENT_BOUND {
                    return Err(MediumError::Chain {
                        rule: "accounting/below-boundary-bound",
                    });
                }
                if !seen.insert(cursor) {
                    return Err(MediumError::Chain {
                        rule: "accounting/below-boundary-cycle",
                    });
                }
                let record = crate::reclaim::read_record_via(reads, &cursor)?;
                sorter.push_protector(
                    cursor,
                    StorageRole::Record,
                    ProtectorKind::BelowBoundaryHistory,
                    cursor,
                )?;
                survivor_entries += 1;
                push_survivor_closure(
                    &mut sorter,
                    reads,
                    cursor,
                    ProtectorKind::BelowBoundaryHistory,
                    &record.content_refs,
                    &mut survivor_entries,
                    &mut composite_closure_roots,
                )?;
                revisions += 1;
                match record.parent {
                    None => {
                        if !matches!(record.kind, crate::record::RecordKind::Founding { .. })
                            || record.epoch != 0
                        {
                            return Err(MediumError::Chain {
                                rule: "accounting/below-boundary-shape",
                            });
                        }
                        break;
                    }
                    Some(parent) => cursor = parent,
                }
            }
            revisions
        } else {
            0
        };
        // The live HEAD-to-MK chain — every canonical revision
        // above the range, read parent-by-parent with
        // hash-verifying reads, NO skip tables, each protected
        // with its closure. The MK is included; the descent stops
        // there (the range itself is the A2 walk's region).
        let mut cursor = head;
        let mut seen: std::collections::BTreeSet<Hash> = std::collections::BTreeSet::new();
        let mut revisions = 0u64;
        let mut guard = 0u64;
        loop {
            guard += 1;
            if guard > ACCOUNTING_DESCENT_BOUND {
                return Err(MediumError::Chain {
                    rule: "accounting/live-chain-bound",
                });
            }
            if !seen.insert(cursor) {
                return Err(MediumError::Chain {
                    rule: "accounting/live-chain-cycle",
                });
            }
            let record = crate::reclaim::read_record_via(reads, &cursor)?;
            sorter.push_protector(
                cursor,
                StorageRole::Record,
                ProtectorKind::LiveChainRevision,
                cursor,
            )?;
            survivor_entries += 1;
            push_survivor_closure(
                &mut sorter,
                reads,
                cursor,
                ProtectorKind::LiveChainRevision,
                &record.content_refs,
                &mut survivor_entries,
                &mut composite_closure_roots,
            )?;
            revisions += 1;
            if cursor == *mk {
                break;
            }
            let Some(parent) = record.parent else {
                return Err(MediumError::Chain {
                    rule: "accounting/live-chain-truncated",
                });
            };
            cursor = parent;
        }
        live_chain_revisions = revisions;
    }
    // (11) THE PRIOR-BATCH AUDIT STREAM — the conditional
    // premise's identity-level evidence: every prior batch's own
    // audited universe (one audit resident at most).
    for batch in &plan.batches {
        if batch.index >= batch_index {
            continue;
        }
        let (_record, _object, prior_ledger) = load_audit(medium, batch.index)?;
        let prior_unit_records: Vec<Hash> =
            prior_ledger.units.iter().map(|unit| unit.record).collect();
        if prior_unit_records != batch.records {
            return Err(MediumError::Chain {
                rule: "accounting/prior-audit-order",
            });
        }
        for unit in &prior_ledger.units {
            sorter.push_prior(unit.record, StorageRole::Record, batch.index)?;
            for object in &unit.objects {
                sorter.push_prior(*object, StorageRole::Object, batch.index)?;
            }
        }
    }
    // (12) THE DETERMINISTIC JOIN — the merged, exact, sorted
    // identity stream classified into the partitions.
    let mut merged = sorter.finish()?;
    let facts = merged.facts();
    let outputs = classify_merged(&mut merged, limits)?;
    let cleanup_ok = merged.cleanup();
    let mandatory = outputs.mandatory;
    let deferred = outputs.deferred;
    let shared = outputs.shared;
    let collisions = outputs.collisions;
    // (13) THE PREMISE AND THE RESULT.
    let prior_batch_premise = if plan
        .batches
        .first()
        .is_some_and(|batch| batch.index == batch_index)
    {
        PriorBatchPremise::FirstBatch
    } else {
        PriorBatchPremise::ConditionalOnPriorBatches {
            batches: plan
                .batches
                .iter()
                .filter(|batch| batch.index < batch_index)
                .map(|batch| batch.index)
                .collect(),
        }
    };
    let counts = AccountingCounts {
        audit_units,
        candidate_identities: (mandatory.len() + deferred.len()) as u64,
        mandatory: mandatory.len() as u64,
        deferred: deferred.len() as u64,
        conditional: mandatory
            .iter()
            .filter(|entry| entry.conditional_on_prior_batches)
            .count() as u64,
        shared_references: shared.len() as u64,
        namespace_collisions: collisions.len() as u64,
    };
    Ok(RetirementAccounting {
        source_mk: *mk,
        selected_batch: batch_index,
        plan_batch_count: plan.batches.len() as u64,
        begin_revision,
        plan_revision,
        audit_revision,
        audit_object,
        ledger_byte_limit: plan.ledger_byte_limit,
        range_digest: plan.range_digest,
        founding_seed: plan.seed,
        mandatory,
        deferred,
        shared_references: shared,
        namespace_collisions: collisions,
        prior_batch_premise,
        survivors: SurvivorRootSummary {
            range_anchors: anchor_ids.len() as u64,
            future_batch_members: future_ids.len() as u64,
            live_chain_revisions,
            below_boundary_revisions,
            boundary: walk_summary.boundary,
            boundary_prior_checkpoint: walk_summary.prior_checkpoint,
            composite_closure_roots,
            survivor_requirement_entries: survivor_entries,
        },
        counts,
        limitations: AccountingLimitations {
            live_coordination: LiveCoordination::OrdinaryReadsWithoutCoordinationBoundary,
            deeper_history: if boundary_prior_checkpoint {
                DeeperHistoryScope::BelowBoundaryWalked {
                    revisions: below_boundary_revisions,
                }
            } else {
                DeeperHistoryScope::FoundingBoundary
            },
            prior_batch_completion_trust: PriorBatchCompletionTrust::Unratified,
        },
        scratch: ScratchReport {
            runs_written: facts.runs_written,
            bytes_written: facts.bytes_written,
            entries_written: facts.entries_written,
            cleanup_residual: !cleanup_ok,
        },
    })
}

// ==================================================== the scratch codec

#[cfg(test)]
mod a3_scratch_tests {
    use super::*;

    fn scratch_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "s7-a3s-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn fabricated(seed: u8) -> Hash {
        let mut bytes = [0u8; 32];
        bytes[0] = 1;
        bytes[31] = seed;
        Hash::from_bytes(&bytes).unwrap()
    }

    fn candidate_entry(seed: u8, unit: u64) -> AccountingEntry {
        AccountingEntry {
            identity: fabricated(seed),
            role: seed % 2,
            source_kind: SOURCE_CANDIDATE_UNIT,
            source: numeric_source(unit),
        }
    }

    fn protector_entry(seed: u8) -> AccountingEntry {
        let mut source = [0u8; 32];
        source[0] = seed;
        AccountingEntry {
            identity: fabricated(seed),
            role: ROLE_OBJECT,
            source_kind: SOURCE_PROTECTOR_ANCHOR,
            source,
        }
    }

    /// The run framing with the keyed transport trailer: the
    /// magic, the version, the count, the entries, and the
    /// position-bound MAC over exactly the preceding bytes.
    fn frame(key: &ScratchKey, stream: u64, entries: &[AccountingEntry]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(ACCOUNTING_RUN_MAGIC);
        bytes.push(ACCOUNTING_RUN_VERSION);
        bytes.extend_from_slice(&(entries.len() as u64).to_be_bytes());
        for entry in entries {
            bytes.extend_from_slice(&encode_entry(entry));
        }
        bytes.extend_from_slice(&transport_mac(key, stream, &bytes));
        bytes
    }

    fn write_run(path: &std::path::Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn assert_rule(error: MediumError, rule: &str) {
        assert!(
            matches!(&error, MediumError::Chain { rule: observed } if *observed == rule),
            "the refusal carries the rule {rule:?}: {error:?}"
        );
    }

    fn open_err(path: &std::path::Path, key: &ScratchKey, stream: u64) -> MediumError {
        match RunReader::open(path, key, stream) {
            Ok(_) => panic!("the run must refuse"),
            Err(error) => error,
        }
    }

    /// (A3-T9, the codec half) The run framing round-trips exactly:
    /// the magic, the version, the count, every fixed-size entry,
    /// and the keyed transport trailer.
    #[test]
    fn the_run_codec_round_trips() {
        let root = scratch_root("codec");
        let key = fresh_scratch_key();
        let entries: Vec<AccountingEntry> = (0..25u8)
            .map(|seed| candidate_entry(seed.wrapping_mul(7), seed as u64))
            .collect();
        let path = root.join("run-000000");
        write_run(&path, &frame(&key, 0, &entries));
        let mut reader = RunReader::open(&path, &key, 0).unwrap();
        let mut read_back = Vec::new();
        while let Some(entry) = reader.current {
            read_back.push(entry);
            reader.advance().unwrap();
        }
        assert_eq!(read_back, entries);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (A3-T9, the codec half) Every malformed scratch framing
    /// refuses typed: the wrong magic, the wrong version, the
    /// truncation, the count/length disagreement, the trailing
    /// bytes, the out-of-range role and source kind, the zero
    /// identity, and the nonzero numeric padding.
    #[test]
    fn the_malformed_run_framing_refuses() {
        let root = scratch_root("malformed");
        let key = fresh_scratch_key();
        let entries = vec![candidate_entry(1, 0), candidate_entry(2, 1)];
        let path = root.join("run-000000");
        // (a) The wrong magic.
        let mut bytes = frame(&key, 0, &entries);
        bytes[0] = b'X';
        write_run(&path, &bytes);
        assert_rule(open_err(&path, &key, 0), "accounting/scratch-framing");
        // (b) The wrong version.
        let mut bytes = frame(&key, 0, &entries);
        bytes[8] = 9;
        write_run(&path, &bytes);
        assert_rule(open_err(&path, &key, 0), "accounting/scratch-framing");
        // (c) The truncation — the first entry decodes, the second
        // is cut short (the refusal lands at the advance).
        let bytes = frame(&key, 0, &entries);
        write_run(&path, &bytes[..bytes.len() - 76]);
        let mut reader = RunReader::open(&path, &key, 0).unwrap();
        let error = match reader.advance() {
            Ok(_) => panic!("the truncated run must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-framing");
        // (d) The count/length disagreement (three declared, two
        // present).
        let mut bytes = frame(&key, 0, &entries);
        bytes[9] = 3;
        write_run(&path, &bytes);
        let mut reader = RunReader::open(&path, &key, 0).unwrap();
        reader.advance().unwrap();
        let error = match reader.advance() {
            Ok(_) => panic!("the count mismatch must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-framing");
        // (e) The trailing bytes.
        let mut bytes = frame(&key, 0, &entries);
        bytes.push(0);
        write_run(&path, &bytes);
        let mut reader = RunReader::open(&path, &key, 0).unwrap();
        reader.advance().unwrap();
        let error = match reader.advance() {
            Ok(_) => panic!("the trailing bytes must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-framing");
        // (f) The out-of-range role.
        let mut bytes = frame(&key, 0, &entries);
        bytes[ACCOUNTING_RUN_HEADER_BYTES + 32] = 9;
        write_run(&path, &bytes);
        assert_rule(open_err(&path, &key, 0), "accounting/scratch-framing");
        // (g) The out-of-range source kind.
        let mut bytes = frame(&key, 0, &entries);
        bytes[ACCOUNTING_RUN_HEADER_BYTES + 33] = 9;
        write_run(&path, &bytes);
        assert_rule(open_err(&path, &key, 0), "accounting/scratch-framing");
        // (h) The zero identity.
        let mut bytes = frame(&key, 0, &entries);
        bytes[ACCOUNTING_RUN_HEADER_BYTES..ACCOUNTING_RUN_HEADER_BYTES + 32].fill(0);
        write_run(&path, &bytes);
        assert_rule(open_err(&path, &key, 0), "accounting/scratch-framing");
        // (i) The nonzero numeric padding.
        let mut bytes = frame(&key, 0, &entries);
        bytes[ACCOUNTING_RUN_HEADER_BYTES + 34 + 10] = 1;
        write_run(&path, &bytes);
        assert_rule(open_err(&path, &key, 0), "accounting/scratch-framing");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (A3-F2) The scratch transport integrity: a run rewritten
    /// between the spill and the merge — even with valid framing
    /// and counts — fails the keyed trailer and refuses typed; a
    /// run file moved to another position fails its position's
    /// recomputation; a correctly-MAC'd but unsorted run refuses
    /// the sorted-order validation; and the survivor-reference
    /// spool carries the same guarantees.
    #[test]
    fn the_scratch_transport_integrity_refuses() {
        let root = scratch_root("integrity");
        let key = fresh_scratch_key();
        let entries = vec![candidate_entry(1, 0), candidate_entry(2, 1)];
        let path = root.join("run-000000");
        // (a) The substitution: the SECOND entry's identity bytes
        // flipped upward (the sorted order is preserved, the
        // count and framing stay correct — only the keyed trailer
        // can catch it).
        let mut bytes = frame(&key, 0, &entries);
        bytes[ACCOUNTING_RUN_HEADER_BYTES + ACCOUNTING_ENTRY_BYTES + 5] ^= 0x40;
        write_run(&path, &bytes);
        let mut reader = RunReader::open(&path, &key, 0).unwrap();
        reader.advance().unwrap();
        let error = match reader.advance() {
            Ok(_) => panic!("the substituted run must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-integrity");
        // (b) The position binding: a run MAC'd for position 1
        // read at position 0 (a renamed or swapped file).
        write_run(&path, &frame(&key, 1, &entries));
        let mut reader = RunReader::open(&path, &key, 0).unwrap();
        reader.advance().unwrap();
        let error = match reader.advance() {
            Ok(_) => panic!("the repositioned run must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-integrity");
        // (c) The unsorted run under a CORRECT MAC: the merge's
        // correctness invariant refuses the order itself.
        let mut unsorted = vec![entries[1], entries[0]];
        unsorted[0].identity = fabricated(9);
        unsorted[1].identity = fabricated(3);
        unsorted.sort_unstable_by(|a, b| b.cmp(a));
        write_run(&path, &frame(&key, 0, &unsorted));
        let mut reader = RunReader::open(&path, &key, 0).unwrap();
        let error = match reader.advance() {
            Ok(_) => panic!("the unsorted run must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-order");
        // (d) The spool: the records round-trip exactly and a
        // substituted spool byte fails the trailer.
        let limits = AccountingLimits {
            run_entry_buffer: 8,
            max_runs: 64,
            max_scratch_bytes: 1 << 20,
            ..AccountingLimits::default()
        };
        let sorter = IdentitySorter::new(&limits, &root).unwrap();
        let mut spool = SurvivorRefSpool::create(&sorter).unwrap();
        spool
            .append(fabricated(1), ProtectorKind::RangeAnchor, &[fabricated(2)])
            .unwrap();
        spool
            .append(
                fabricated(3),
                ProtectorKind::FutureBatchMember,
                &[fabricated(4), fabricated(5)],
            )
            .unwrap();
        let spool_key = sorter.key;
        let mut reader = spool.into_reader().unwrap();
        let mut observed = Vec::new();
        while let Some(item) = reader.next_reference().unwrap() {
            observed.push(item);
        }
        assert_eq!(observed.len(), 3);
        assert_eq!(observed[0].2, fabricated(2));
        assert_eq!(observed[1].2, fabricated(4));
        assert_eq!(observed[2].2, fabricated(5));
        assert!(!observed[2].3);
        assert!(observed[0].3, "the first reference of each record");
        // The substituted spool: one reference byte flipped.
        let sorter = IdentitySorter::new(&limits, &root).unwrap();
        let mut spool = SurvivorRefSpool::create(&sorter).unwrap();
        spool
            .append(fabricated(1), ProtectorKind::RangeAnchor, &[fabricated(2)])
            .unwrap();
        let spool_path = sorter.dir.join(SURVIVOR_SPOOL_FILE);
        let trailer: [u8; 32] = spool.mac.clone().finalize().into();
        spool
            .writer
            .write_all(&trailer)
            .and_then(|_| spool.writer.flush())
            .unwrap();
        drop(spool);
        let mut bytes = std::fs::read(&spool_path).unwrap();
        bytes[SPOOL_RECORD_HEADER_BYTES + 7] ^= 0x40;
        std::fs::write(&spool_path, &bytes).unwrap();
        // The flipped byte lands inside the reference's identity:
        // the record still decodes (the framing and the count are
        // untouched), and ONLY the keyed trailer comparison at the
        // end refuses.
        let mut reader = SurvivorRefReader::open(&spool_path, spool_key).unwrap();
        assert!(
            reader.next_reference().is_ok(),
            "the substituted reference still decodes"
        );
        let error = match reader.next_reference() {
            Ok(_) => panic!("the substituted spool must refuse at the trailer"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-integrity");
        drop(sorter);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (A3-F2) The workspace exclusivity: a planted entry in the
    /// scratch workspace (a rogue file beside the expected runs)
    /// refuses the merge before any run is consumed.
    #[test]
    fn the_scratch_workspace_refuses_unexpected_entries() {
        let root = scratch_root("workspace");
        let limits = AccountingLimits {
            run_entry_buffer: 2,
            max_runs: 64,
            max_scratch_bytes: 1 << 20,
            ..AccountingLimits::default()
        };
        let mut sorter = IdentitySorter::new(&limits, &root).unwrap();
        let mut spool = SurvivorRefSpool::create(&sorter).unwrap();
        spool
            .append(fabricated(1), ProtectorKind::RangeAnchor, &[fabricated(2)])
            .unwrap();
        drop(spool);
        for seed in 0..3u8 {
            sorter.push(candidate_entry(seed, 0)).unwrap();
        }
        // The planted rogue entry.
        let workspace = sorter.dir.clone();
        std::fs::write(workspace.join("run-000001.bak"), b"rogue").unwrap();
        let error = match sorter.finish() {
            Ok(_) => panic!("the rogue workspace entry must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-integrity");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (A3-T9, the merge half) The forced-small spill threshold
    /// produces many runs, the duplicates crossing run boundaries
    /// collapse exactly, and the merged stream equals the sorted
    /// deduplicated push multiset — the ordering never depends on
    /// the run boundaries.
    #[test]
    fn the_tiny_buffer_spill_merges_deterministically() {
        let root = scratch_root("spill");
        let limits = AccountingLimits {
            run_entry_buffer: 3,
            max_runs: 64,
            max_scratch_bytes: 1 << 20,
            ..AccountingLimits::default()
        };
        let mut sorter = IdentitySorter::new(&limits, &root).unwrap();
        let mut spool = SurvivorRefSpool::create(&sorter).unwrap();
        spool
            .append(fabricated(1), ProtectorKind::RangeAnchor, &[fabricated(2)])
            .unwrap();
        drop(spool);
        let mut pushed: Vec<AccountingEntry> = Vec::new();
        for seed in 0..40u8 {
            let entry = candidate_entry(seed, (seed % 5) as u64);
            pushed.push(entry);
            sorter.push(entry).unwrap();
            if seed % 3 == 0 {
                // The exact duplicate — the merge collapses it.
                sorter.push(entry).unwrap();
            }
            if seed % 4 == 0 {
                let protector = protector_entry(seed);
                pushed.push(protector);
                sorter.push(protector).unwrap();
            }
        }
        let mut merged = sorter.finish().unwrap();
        let facts = merged.facts();
        assert!(
            facts.runs_written >= 8,
            "the tiny buffer forces many runs: {facts:?}"
        );
        let mut stream = Vec::new();
        while let Some(entry) = merged.next().unwrap() {
            stream.push(entry);
        }
        assert!(merged.cleanup());
        pushed.sort_unstable();
        pushed.dedup();
        assert_eq!(stream, pushed);
        let residual: Vec<_> = std::fs::read_dir(&root).unwrap().collect();
        assert!(residual.is_empty(), "the scratch directory was removed");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (A3-F3) The pathological limits refuse BEFORE any
    /// allocation or scratch file exists, the resource ceilings
    /// refuse typed when crossed, and a mid-computation failure
    /// after spilling cleans the workspace.
    #[test]
    fn the_pathological_limits_and_ceilings_refuse() {
        let root = scratch_root("limits");
        // (a) The excessive buffer capacity (above the hard
        // maximum) refuses before the directory exists.
        let excessive = AccountingLimits {
            run_entry_buffer: MAX_ACCOUNTING_RUN_BUFFER_ENTRIES + 1,
            ..AccountingLimits::default()
        };
        let error = match IdentitySorter::new(&excessive, &root.join("a")) {
            Ok(_) => panic!("the excessive buffer must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/limits");
        // (b) Each field's pathological form.
        for limits in [
            AccountingLimits {
                run_entry_buffer: 0,
                ..AccountingLimits::default()
            },
            AccountingLimits {
                max_runs: MAX_ACCOUNTING_RUN_COUNT + 1,
                ..AccountingLimits::default()
            },
            AccountingLimits {
                max_scratch_bytes: MAX_ACCOUNTING_SCRATCH_BYTE_CEILING + 1,
                ..AccountingLimits::default()
            },
            AccountingLimits {
                max_mandatory_identities: MAX_ACCOUNTING_CLASSIFIED_LIST + 1,
                ..AccountingLimits::default()
            },
            AccountingLimits {
                max_shared_references: 0,
                ..AccountingLimits::default()
            },
            AccountingLimits {
                max_namespace_collisions: MAX_ACCOUNTING_COLLISION_LIST + 1,
                ..AccountingLimits::default()
            },
            AccountingLimits {
                max_protectors_per_deferred: 0,
                ..AccountingLimits::default()
            },
        ] {
            let error = match IdentitySorter::new(&limits, &root.join("b")) {
                Ok(_) => panic!("the pathological limit must refuse"),
                Err(error) => error,
            };
            assert_rule(error, "accounting/limits");
        }
        // (c) The extreme VALID boundary behaves correctly (the
        // checked arithmetic never wraps or panics).
        let extreme = AccountingLimits {
            run_entry_buffer: MAX_ACCOUNTING_RUN_BUFFER_ENTRIES,
            max_runs: MAX_ACCOUNTING_RUN_COUNT,
            max_scratch_bytes: MAX_ACCOUNTING_SCRATCH_BYTE_CEILING,
            max_mandatory_identities: MAX_ACCOUNTING_CLASSIFIED_LIST,
            max_deferred_identities: MAX_ACCOUNTING_CLASSIFIED_LIST,
            max_shared_references: MAX_ACCOUNTING_SHARED_LIST,
            max_namespace_collisions: MAX_ACCOUNTING_COLLISION_LIST,
            max_protectors_per_deferred: MAX_ACCOUNTING_PROTECTOR_LIST,
        };
        let mut sorter = IdentitySorter::new(&extreme, &root.join("c")).unwrap();
        let mut spool = SurvivorRefSpool::create(&sorter).unwrap();
        spool
            .append(fabricated(1), ProtectorKind::RangeAnchor, &[fabricated(2)])
            .unwrap();
        drop(spool);
        sorter.push(candidate_entry(1, 0)).unwrap();
        let mut merged = sorter.finish().unwrap();
        assert!(merged.next().unwrap().is_some());
        assert!(merged.cleanup());
        // (d) The run-count ceiling refused mid-computation after
        // spilling, with the workspace cleaned on the failing path.
        let limits = AccountingLimits {
            run_entry_buffer: 2,
            max_runs: 1,
            max_scratch_bytes: 1 << 20,
            ..AccountingLimits::default()
        };
        let sub = root.join("d");
        let mut sorter = IdentitySorter::new(&limits, &sub).unwrap();
        let mut spool = SurvivorRefSpool::create(&sorter).unwrap();
        spool
            .append(fabricated(1), ProtectorKind::RangeAnchor, &[fabricated(2)])
            .unwrap();
        drop(spool);
        for seed in 0..3u8 {
            sorter.push(candidate_entry(seed, 0)).unwrap();
        }
        let error = match sorter.finish() {
            Ok(_) => panic!("the run ceiling must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-run-limit");
        let residual: Vec<_> = std::fs::read_dir(&sub).unwrap().collect();
        assert!(
            residual.is_empty(),
            "the failing path cleaned the workspace"
        );
        // (e) The byte ceiling likewise.
        let single_run = (ACCOUNTING_RUN_HEADER_BYTES + 2 * ACCOUNTING_ENTRY_BYTES) as u64
            + ACCOUNTING_RUN_MAC_BYTES as u64;
        let limits = AccountingLimits {
            run_entry_buffer: 2,
            max_runs: 64,
            max_scratch_bytes: single_run,
            ..AccountingLimits::default()
        };
        let sub = root.join("e");
        let mut sorter = IdentitySorter::new(&limits, &sub).unwrap();
        let mut spool = SurvivorRefSpool::create(&sorter).unwrap();
        spool
            .append(fabricated(1), ProtectorKind::RangeAnchor, &[fabricated(2)])
            .unwrap();
        drop(spool);
        for seed in 0..3u8 {
            sorter.push(candidate_entry(seed, 0)).unwrap();
        }
        let error = match sorter.finish() {
            Ok(_) => panic!("the byte ceiling must refuse"),
            Err(error) => error,
        };
        assert_rule(error, "accounting/scratch-byte-limit");
        let residual: Vec<_> = std::fs::read_dir(&sub).unwrap().collect();
        assert!(
            residual.is_empty(),
            "the failing path cleaned the workspace"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

// ================================================ the campaign regressions

#[cfg(all(test, feature = "composite-test-fixture"))]
mod a3_tests {
    use super::*;
    use crate::medium::{CanonicalMedium, StoredObject};

    fn temp_dir_path(prefix: &str, label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "s7-a3-{prefix}-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[cfg(unix)]
    fn a3_store(label: &str) -> (std::path::PathBuf, crate::LocalFileMedium) {
        let root = temp_dir_path("store", label);
        std::fs::create_dir_all(&root).unwrap();
        (root.clone(), crate::LocalFileMedium::open(&root).unwrap())
    }

    fn a3_sqlite_store(label: &str) -> (std::path::PathBuf, crate::SqliteMedium) {
        let path = temp_dir_path("sqlite", label).with_extension("db");
        let medium = crate::SqliteMedium::open(&path).unwrap();
        (path, medium)
    }

    fn a3_sqlite_cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    }

    fn a3_journal(label: &str) -> std::path::PathBuf {
        temp_dir_path("journal", label)
    }

    fn a3_scratch(label: &str) -> std::path::PathBuf {
        temp_dir_path("scratch", label)
    }

    fn cleanup(paths: &[std::path::PathBuf]) {
        for path in paths {
            let _ = std::fs::remove_dir_all(path);
        }
    }

    fn plain_object(payload: &[u8]) -> StoredObject {
        StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(payload);
            enc.into_bytes()
        })
        .unwrap()
    }

    fn a3_member_key(label: &str, index: usize) -> Vec<u8> {
        format!("a3-{label}-member-{index:04}").into_bytes()
    }

    /// The store's complete file inventory (the read-only proof's
    /// basis — the a2 discipline).
    #[cfg(unix)]
    fn inventory(root: &std::path::Path) -> Vec<(std::path::PathBuf, u64)> {
        let mut files = Vec::new();
        if root.is_file() {
            files.push((root.to_path_buf(), std::fs::metadata(root).unwrap().len()));
            return files;
        }
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let size = entry.metadata().unwrap().len();
                    files.push((path, size));
                }
            }
        }
        files.sort();
        files
    }

    // ------------------------------------------------- the fixture

    /// One bespoke chain step inside the checkpoint range.
    enum A3Step {
        /// A retiring member (indexed): its chain-unique key and
        /// its content objects.
        Member {
            key: Vec<u8>,
            objects: Vec<StoredObject>,
        },
        /// A retained anchor (unindexed): its content objects.
        #[cfg_attr(not(unix), allow(dead_code))]
        Anchor { objects: Vec<StoredObject> },
        /// The reintroduction anchor: a retained anchor whose single
        /// content reference is the PREVIOUS member's record
        /// identity, with the record's canonical bytes re-supplied
        /// as the ordinary object (the namespace-collision fixture).
        #[cfg_attr(not(unix), allow(dead_code))]
        Reintroduce,
    }

    fn member_step(key: Vec<u8>, objects: Vec<StoredObject>) -> A3Step {
        A3Step::Member { key, objects }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    fn anchor_step(objects: Vec<StoredObject>) -> A3Step {
        A3Step::Anchor { objects }
    }

    /// The composite snapshot published ABOVE the MK (on the live
    /// chain, before the plan): the surviving composite root whose
    /// verified closure protects its dependencies.
    struct A3AboveComposite {
        root: StoredObject,
        new_chunks: Vec<StoredObject>,
        reused_chunks: Vec<Hash>,
    }

    struct A3Facts {
        mk: Hash,
        sk: Hash,
        founding: Hash,
        p1: Hash,
        shared: Hash,
        members: Vec<Hash>,
        member_objects: Vec<Vec<Hash>>,
        anchors: Vec<Hash>,
        anchor_objects: Vec<Vec<Hash>>,
        prior_mk: Option<Hash>,
        prior_m0: Option<Hash>,
        prior_sk: Option<Hash>,
        #[cfg_attr(not(unix), allow(dead_code))]
        above_mk_composite: Option<Hash>,
    }

    fn a3_step_append(
        medium: &mut dyn CanonicalMedium,
        objects: &[StoredObject],
        tick: u64,
    ) -> Hash {
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let refs: Vec<Hash> = objects.iter().map(|object| object.hash()).collect();
        let record =
            crate::record::RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
        medium.append(&record, objects, Some(head)).unwrap();
        record.id()
    }

    /// The bespoke campaign (the accepted a12 shape generalized):
    /// the founding with the shared object, the bound journal, p1,
    /// the optional genuine prior checkpoint (the F2-B shape),
    /// the caller's steps (members and anchors interleaved), SK,
    /// the checkpoint metadata, the MK, the optional above-MK
    /// composite snapshot, the plan, and EVERY plan-defined audit.
    /// The begin and the witness are the caller's (the tier policy
    /// differs by provider).
    fn a3_campaign(
        medium: &mut dyn CanonicalMedium,
        label: &str,
        journal_root: &std::path::Path,
        steps: &[A3Step],
        ledger_limit: u64,
        with_prior_checkpoint: bool,
        above_mk: Option<&A3AboveComposite>,
    ) -> A3Facts {
        let shared = plain_object(format!("a3-{label}-shared").as_bytes());
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            label,
            0,
        )
        .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(journal_root);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        drop(
            crate::reclaim::create_campaign_bound_journal(medium, journal_root, &founding_id)
                .unwrap(),
        );
        let (_binding, _outcome) = crate::reclaim::publish_journal_root_binding(
            medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        let p1 = a3_step_append(medium, std::slice::from_ref(&shared), 2);
        // The optional genuine prior checkpoint below the current
        // range (the F2-B accepted shape: its own member, its own
        // SK, and its MK — a regular revision on the SK, the
        // ratified deterministic key, the strictly coherent
        // metadata).
        let (prior_mk, prior_m0, prior_sk) = if with_prior_checkpoint {
            let prior_m0 = a3_step_append(medium, std::slice::from_ref(&shared), 3);
            let prior_m0_epoch = medium.read_record(&prior_m0).unwrap().epoch;
            let prior_m0_bytes = medium.read_record(&prior_m0).unwrap().canonical_bytes();
            let prior_sk = a3_step_append(medium, std::slice::from_ref(&shared), 4);
            let prior_sk_epoch = medium.read_record(&prior_sk).unwrap().epoch;
            let prior_seed = crate::checkpoint::compute_digest_seed(&founding_id);
            let prior_digest =
                crate::checkpoint::chain_range_digest(&prior_seed, &[prior_m0_bytes]);
            let prior_metadata = crate::checkpoint::CheckpointMetadata::new(
                prior_sk,
                prior_m0,
                vec![crate::checkpoint::SkipEntry::new(Hash::ZERO, p1)],
                vec![
                    crate::checkpoint::ReclaimedIndexEntry::new(None, prior_m0, prior_m0_epoch),
                    crate::checkpoint::ReclaimedIndexEntry::new(None, prior_sk, prior_sk_epoch),
                ],
                prior_digest,
                prior_seed,
            );
            let prior_metadata_object =
                StoredObject::new(prior_metadata.canonical_bytes()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let prior_epoch = medium.read_record(&head).unwrap().epoch;
            let prior_mk_record = crate::record::RevisionRecord::regular(prior_sk, prior_epoch, 5)
                .with_idempotency_key(crate::checkpoint::checkpoint_mk_idempotency_key(&prior_sk))
                .with_content_refs(vec![prior_metadata_object.hash()]);
            medium
                .append(
                    &prior_mk_record,
                    std::slice::from_ref(&prior_metadata_object),
                    Some(prior_sk),
                )
                .unwrap();
            (Some(prior_mk_record.id()), Some(prior_m0), Some(prior_sk))
        } else {
            (None, None, None)
        };
        // The caller's steps.
        let mut members: Vec<Hash> = Vec::new();
        let mut member_objects: Vec<Vec<Hash>> = Vec::new();
        let mut anchors: Vec<Hash> = Vec::new();
        let mut anchor_objects: Vec<Vec<Hash>> = Vec::new();
        for step in steps {
            match step {
                A3Step::Member { key, objects } => {
                    let head = medium.head().unwrap().unwrap();
                    let epoch = medium.read_record(&head).unwrap().epoch;
                    let refs: Vec<Hash> = objects.iter().map(|object| object.hash()).collect();
                    let record = crate::record::RevisionRecord::regular(head, epoch, 6)
                        .with_idempotency_key(key.clone())
                        .with_content_refs(refs);
                    medium.append(&record, objects, Some(head)).unwrap();
                    members.push(record.id());
                    member_objects.push(objects.iter().map(|object| object.hash()).collect());
                }
                A3Step::Anchor { objects } => {
                    let head = medium.head().unwrap().unwrap();
                    let epoch = medium.read_record(&head).unwrap().epoch;
                    let refs: Vec<Hash> = objects.iter().map(|object| object.hash()).collect();
                    let record = crate::record::RevisionRecord::regular(head, epoch, 6)
                        .with_content_refs(refs);
                    medium.append(&record, objects, Some(head)).unwrap();
                    anchors.push(record.id());
                    anchor_objects.push(objects.iter().map(|object| object.hash()).collect());
                }
                A3Step::Reintroduce => {
                    let previous = *members
                        .last()
                        .expect("the reintroduction anchor follows a member");
                    let bytes = medium.read_record(&previous).unwrap().canonical_bytes();
                    let object = StoredObject::new(bytes).unwrap();
                    assert_eq!(
                        object.hash(),
                        previous,
                        "the record's canonical bytes hash to its identity"
                    );
                    let head = medium.head().unwrap().unwrap();
                    let epoch = medium.read_record(&head).unwrap().epoch;
                    let record = crate::record::RevisionRecord::regular(head, epoch, 6)
                        .with_content_refs(vec![previous]);
                    medium
                        .append(&record, std::slice::from_ref(&object), Some(head))
                        .unwrap();
                    anchors.push(record.id());
                    anchor_objects.push(vec![previous]);
                }
            }
        }
        // SK — the range's terminal self-retiring record.
        let sk = a3_step_append(medium, std::slice::from_ref(&shared), 7);
        let sk_record = medium.read_record(&sk).unwrap();
        // The checkpoint metadata: the TRUE record epochs and keys,
        // the ascending range digest over the members EXCLUDING SK,
        // and the self-skip to the retained ancestor below the
        // range (the prior MK when it exists, else p1).
        let seed = crate::checkpoint::compute_digest_seed(&founding_id);
        let mut digest = seed;
        let mut index = Vec::with_capacity(members.len() + 1);
        for id in &members {
            let record = medium.read_record(id).unwrap();
            digest = crate::checkpoint::chain_range_digest_step(&digest, &record.canonical_bytes());
            index.push(crate::checkpoint::ReclaimedIndexEntry::new(
                record.idempotency_key.clone(),
                *id,
                record.epoch,
            ));
        }
        index.push(crate::checkpoint::ReclaimedIndexEntry::new(
            sk_record.idempotency_key.clone(),
            sk,
            sk_record.epoch,
        ));
        let skip_target = prior_mk.unwrap_or(p1);
        let metadata = crate::checkpoint::CheckpointMetadata::new(
            sk,
            sk_record.parent.unwrap_or(skip_target),
            vec![crate::checkpoint::SkipEntry::new(Hash::ZERO, skip_target)],
            index,
            digest,
            seed,
        );
        let metadata_object = StoredObject::new(metadata.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk_record = crate::record::RevisionRecord::regular(head, epoch, 8)
            .with_content_refs(vec![metadata_object.hash()]);
        medium
            .append(
                &mk_record,
                std::slice::from_ref(&metadata_object),
                Some(head),
            )
            .unwrap();
        let mk = mk_record.id();
        // The optional above-MK composite snapshot (the live-chain
        // surviving composite root).
        let above_mk_composite = if let Some(spec) = above_mk {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = crate::record::RevisionRecord::regular(head, epoch, 9)
                .with_content_refs(vec![spec.root.hash()]);
            let request = crate::composite_append::CompositeAppend {
                record: &record,
                root: &spec.root,
                new_chunks: &spec.new_chunks,
                reused_chunks: &spec.reused_chunks,
            };
            medium.append_composite(&request, Some(head)).unwrap();
            Some(record.id())
        } else {
            None
        };
        // The plan and EVERY plan-defined audit.
        crate::reclaim::publish_reclamation_plan(medium, &mk, ledger_limit, journal_root, 10)
            .unwrap();
        let (_plan_id, plan) = crate::reclaim::find_installed_plan(medium, &mk)
            .unwrap()
            .unwrap();
        for batch in &plan.batches {
            crate::reclaim::publish_reclamation_ledger_v2(medium, &mk, batch.index, 11).unwrap();
        }
        A3Facts {
            mk,
            sk,
            founding: founding_id,
            p1,
            shared: shared.hash(),
            members,
            member_objects,
            anchors,
            anchor_objects,
            prior_mk,
            prior_m0,
            prior_sk,
            above_mk_composite,
        }
    }

    /// The campaign completion (the caller's tier policy): the CP2
    /// begin and the A1-2 witness publication.
    fn a3_complete(medium: &mut dyn CanonicalMedium, mk: &Hash) {
        medium.begin_reclamation_v2(mk, 12).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(medium, mk, 13).unwrap();
    }

    fn account(
        medium: &mut dyn CanonicalMedium,
        mk: &Hash,
        batch: u64,
        limits: &AccountingLimits,
        scratch: &std::path::Path,
    ) -> RetirementAccounting {
        account_batch_retirement(medium, mk, batch, limits, scratch).unwrap()
    }

    fn prot(kind: ProtectorKind, root: Hash) -> ProtectorRef {
        ProtectorRef { kind, root }
    }

    /// The independently constructed expected partition (from the
    /// fixture's construction facts and the installed plan — never
    /// from the implementation's outputs): the global-first-seen
    /// candidate attribution, the exact survivor protection over
    /// the fixture's known reference sets, and the prior-batch
    /// conditional flags.
    fn derive_expected(
        facts: &A3Facts,
        plan: &crate::reclaim::ReclaimPlan,
        selected: u64,
    ) -> (
        Vec<MandatoryIdentity>,
        Vec<DeferredIdentity>,
        Vec<SharedReference>,
    ) {
        let mut objects_of: std::collections::BTreeMap<Hash, Vec<Hash>> = facts
            .members
            .iter()
            .zip(facts.member_objects.iter())
            .map(|(identity, objects)| (*identity, objects.clone()))
            .collect();
        // SK is an indexed member too (the self-retiring top): its
        // audited object is the shared object.
        objects_of.insert(facts.sk, vec![facts.shared]);
        let selected_batch = plan
            .batches
            .iter()
            .find(|batch| batch.index == selected)
            .unwrap();
        let prior_members: Vec<Hash> = plan
            .batches
            .iter()
            .filter(|batch| batch.index < selected)
            .flat_map(|batch| batch.records.iter().copied())
            .collect();
        let future_members: Vec<Hash> = plan
            .batches
            .iter()
            .filter(|batch| batch.index > selected)
            .flat_map(|batch| batch.records.iter().copied())
            .collect();
        let prior_identities: std::collections::BTreeSet<Hash> = prior_members
            .iter()
            .flat_map(|identity| {
                let mut all = objects_of.get(identity).cloned().unwrap_or_default();
                all.push(*identity);
                all
            })
            .collect();
        // The survivor requirement sets (the fixture's known
        // shape): the anchors' and future members' objects, and the
        // shared object protected by the boundary side (the
        // founding, or the below-boundary history when a prior
        // checkpoint exists).
        let mut survivor_objects: std::collections::BTreeSet<Hash> = future_members
            .iter()
            .flat_map(|identity| objects_of.get(identity).cloned().unwrap_or_default())
            .collect();
        let anchor_objects: Vec<(Hash, Vec<Hash>)> = facts
            .anchors
            .iter()
            .zip(facts.anchor_objects.iter())
            .map(|(identity, objects)| (*identity, objects.clone()))
            .collect();
        for (_identity, objects) in &anchor_objects {
            survivor_objects.extend(objects.iter().copied());
        }
        survivor_objects.insert(facts.shared);
        let founding_case = facts.prior_mk.is_none();
        // The candidates, per (identity, role): the first unit and
        // every referencing unit.
        let mut first_unit: std::collections::BTreeMap<(Hash, StorageRole), u64> =
            std::collections::BTreeMap::new();
        let mut referencing: std::collections::BTreeMap<(Hash, StorageRole), Vec<u64>> =
            std::collections::BTreeMap::new();
        for (unit, record) in selected_batch.records.iter().enumerate() {
            let unit = unit as u64;
            first_unit
                .entry((*record, StorageRole::Record))
                .or_insert(unit);
            referencing
                .entry((*record, StorageRole::Record))
                .or_default()
                .push(unit);
            for object in objects_of.get(record).unwrap() {
                first_unit
                    .entry((*object, StorageRole::Object))
                    .or_insert(unit);
                referencing
                    .entry((*object, StorageRole::Object))
                    .or_default()
                    .push(unit);
            }
        }
        let mut expected_mandatory = Vec::new();
        let mut expected_deferred = Vec::new();
        let mut expected_shared = Vec::new();
        for ((identity, role), first) in &first_unit {
            let units = &referencing[&(*identity, *role)];
            if *role == StorageRole::Object && survivor_objects.contains(identity) {
                let mut protectors = std::collections::BTreeSet::new();
                for (anchor, objects) in &anchor_objects {
                    if objects.contains(identity) {
                        protectors.insert(prot(ProtectorKind::RangeAnchor, *anchor));
                    }
                }
                for member in &future_members {
                    if objects_of
                        .get(member)
                        .is_some_and(|objects| objects.contains(identity))
                    {
                        protectors.insert(prot(ProtectorKind::FutureBatchMember, *member));
                    }
                }
                if founding_case && *identity == facts.shared {
                    protectors.insert(prot(ProtectorKind::RangeAnchor, facts.p1));
                    protectors.insert(prot(ProtectorKind::RangeBoundary, facts.founding));
                }
                if !founding_case && *identity == facts.shared {
                    for root in [
                        facts.prior_sk.expect("the prior checkpoint exists"),
                        facts.prior_m0.expect("the prior checkpoint exists"),
                        facts.p1,
                        facts.founding,
                    ] {
                        protectors.insert(prot(ProtectorKind::BelowBoundaryHistory, root));
                    }
                }
                expected_deferred.push(DeferredIdentity {
                    identity: *identity,
                    role: *role,
                    first_seen_unit: *first,
                    namespace_collision: false,
                    protectors: protectors.into_iter().collect(),
                });
            } else {
                expected_mandatory.push(MandatoryIdentity {
                    identity: *identity,
                    role: *role,
                    first_seen_unit: *first,
                    conditional_on_prior_batches: prior_identities.contains(identity),
                });
            }
            if units.len() >= 2 {
                expected_shared.push(SharedReference {
                    identity: *identity,
                    role: *role,
                    referencing_units: units.clone(),
                });
            }
        }
        (expected_mandatory, expected_deferred, expected_shared)
    }

    // ------------------------------------------------- the regressions

    /// (A3-T1) The single-batch exact partition: a valid
    /// checkpoint, plan, audit, begin, witness, and A2 ancestry;
    /// the exact candidate identities, survivor protections,
    /// Mandatory set, and Deferred set compared against an
    /// independently constructed expected partition.
    #[cfg(unix)]
    #[test]
    fn the_single_batch_exact_partition() {
        let (root, mut medium) = a3_store("t1");
        let journal = a3_journal("t1");
        let scratch = a3_scratch("t1");
        let facts = a3_campaign(
            &mut medium,
            "t1",
            &journal,
            &[
                member_step(
                    a3_member_key("t1", 0),
                    vec![plain_object(b"a3-t1-payload-m0")],
                ),
                member_step(
                    a3_member_key("t1", 1),
                    vec![plain_object(b"a3-t1-payload-m1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        let result = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let (_plan_record, plan) = crate::reclaim::find_installed_plan(&mut medium, &facts.mk)
            .unwrap()
            .unwrap();
        assert_eq!(plan.batches.len(), 1, "the single-batch fixture");
        let (expected_mandatory, expected_deferred, expected_shared) =
            derive_expected(&facts, &plan, 0);
        assert_eq!(result.mandatory, expected_mandatory);
        assert_eq!(result.deferred, expected_deferred);
        assert_eq!(result.shared_references, expected_shared);
        // The single deferred identity is the shared object (SK's
        // audited unit object), protected by the retained anchor p1
        // and the founding boundary, attributed to unit 0.
        assert_eq!(result.deferred.len(), 1);
        assert_eq!(result.deferred[0].identity, facts.shared);
        assert_eq!(result.deferred[0].role, StorageRole::Object);
        assert_eq!(result.deferred[0].first_seen_unit, 0);
        assert_eq!(
            result.deferred[0].protectors,
            vec![
                prot(ProtectorKind::RangeAnchor, facts.p1),
                prot(ProtectorKind::RangeBoundary, facts.founding),
            ]
        );
        // The premise, the counts, and the survivors.
        assert_eq!(result.prior_batch_premise, PriorBatchPremise::FirstBatch);
        assert_eq!(result.selected_batch, 0);
        assert_eq!(result.plan_batch_count, 1);
        assert_eq!(result.counts.audit_units, 3);
        assert_eq!(result.counts.candidate_identities, 6);
        assert_eq!(result.counts.mandatory, 5);
        assert_eq!(result.counts.deferred, 1);
        assert_eq!(result.counts.conditional, 0);
        assert_eq!(result.counts.shared_references, 0);
        assert_eq!(result.counts.namespace_collisions, 0);
        assert_eq!(result.survivors.range_anchors, 2);
        assert_eq!(result.survivors.future_batch_members, 0);
        assert_eq!(result.survivors.boundary, facts.founding);
        assert!(!result.survivors.boundary_prior_checkpoint);
        assert_eq!(result.survivors.below_boundary_revisions, 0);
        assert_eq!(result.survivors.composite_closure_roots, 0);
        // The exact membership verification surface: the right
        // classification, the wrong role refused, the non-candidate
        // refused.
        assert_eq!(
            result.classification_of(&facts.members[0], StorageRole::Record),
            Some(Classification::Mandatory {
                conditional_on_prior_batches: false
            })
        );
        assert_eq!(
            result.classification_of(&facts.shared, StorageRole::Record),
            None
        );
        assert_eq!(
            result.classification_of(&facts.p1, StorageRole::Record),
            None
        );
        assert!(result.contains(&facts.shared, StorageRole::Object));
        // The explicit limitations: no live-state certification, no
        // deeper history, the unratified completion trust.
        assert_eq!(
            result.limitations.live_coordination,
            LiveCoordination::OrdinaryReadsWithoutCoordinationBoundary
        );
        assert_eq!(
            result.limitations.deeper_history,
            DeeperHistoryScope::FoundingBoundary
        );
        assert_eq!(
            result.limitations.prior_batch_completion_trust,
            PriorBatchCompletionTrust::Unratified
        );
        // The bounded, cleaned scratch.
        assert!(result.scratch.runs_written >= 1);
        assert!(!result.scratch.cleanup_residual);
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T2) The multi-batch interleaving: at least three batches
    /// with retained anchors between indexed members; the exact
    /// partition at EVERY evaluation position; the future-batch
    /// members and retained anchors protected; the flattened plan
    /// list alone demonstrably insufficient; the prior-batch status
    /// never certified completion; and the below-boundary history
    /// of a genuine prior checkpoint walked and protected.
    #[cfg(unix)]
    #[test]
    fn the_multi_batch_positions_and_below_boundary() {
        let (root, mut medium) = a3_store("t2");
        let journal = a3_journal("t2");
        let scratch = a3_scratch("t2");
        // The cross-batch shared object X: referenced by the OLDEST
        // member (m0 — always the LAST batch) and the NEWEST member
        // (m6 — an earlier batch), so the two manifestation
        // positions are deterministic regardless of the exact
        // partition.
        let x = plain_object(b"a3-t2-cross-batch-shared");
        let steps: Vec<A3Step> = vec![
            member_step(a3_member_key("t2", 0), vec![x.clone()]),
            member_step(a3_member_key("t2", 1), vec![plain_object(b"a3-t2-own-1")]),
            anchor_step(vec![plain_object(b"a3-t2-anchor-1")]),
            member_step(a3_member_key("t2", 2), vec![plain_object(b"a3-t2-own-2")]),
            member_step(a3_member_key("t2", 3), vec![plain_object(b"a3-t2-own-3")]),
            anchor_step(vec![plain_object(b"a3-t2-anchor-2")]),
            member_step(a3_member_key("t2", 4), vec![plain_object(b"a3-t2-own-4")]),
            member_step(a3_member_key("t2", 5), vec![plain_object(b"a3-t2-own-5")]),
            member_step(a3_member_key("t2", 6), vec![x.clone()]),
        ];
        let facts = a3_campaign(&mut medium, "t2", &journal, &steps, 500, false, None);
        a3_complete(&mut medium, &facts.mk);
        let (_plan_record, plan) = crate::reclaim::find_installed_plan(&mut medium, &facts.mk)
            .unwrap()
            .unwrap();
        assert!(
            plan.batches.len() >= 3,
            "the shrunken ledger limit partitions the plan: {:?}",
            plan.batches.len()
        );
        let batch_of = |identity: &Hash| {
            plan.batches
                .iter()
                .find(|batch| batch.records.contains(identity))
                .unwrap()
                .index
        };
        let newest_batch = batch_of(&facts.members[6]);
        let oldest_batch = batch_of(&facts.members[0]);
        assert!(
            oldest_batch > newest_batch,
            "the oldest member lands in a later batch than the newest"
        );
        // The exact partition at EVERY position.
        for batch in &plan.batches {
            let result = account(
                &mut medium,
                &facts.mk,
                batch.index,
                &AccountingLimits::default(),
                &scratch,
            );
            let (expected_mandatory, expected_deferred, expected_shared) =
                derive_expected(&facts, &plan, batch.index);
            assert_eq!(
                result.mandatory, expected_mandatory,
                "the mandatory partition at batch {}",
                batch.index
            );
            assert_eq!(
                result.deferred, expected_deferred,
                "the deferred partition at batch {}",
                batch.index
            );
            assert_eq!(
                result.shared_references, expected_shared,
                "the shared references at batch {}",
                batch.index
            );
            let expected_premise = if batch.index == 0 {
                PriorBatchPremise::FirstBatch
            } else {
                PriorBatchPremise::ConditionalOnPriorBatches {
                    batches: (0..batch.index).collect(),
                }
            };
            assert_eq!(
                result.prior_batch_premise, expected_premise,
                "the premise at batch {}",
                batch.index
            );
        }
        // (a) The FUTURE-batch protection manifests: selecting the
        // newest member's batch, the cross-batch shared object X is
        // a current candidate protected by the oldest member (a
        // FUTURE member of a later batch).
        let result = account(
            &mut medium,
            &facts.mk,
            newest_batch,
            &AccountingLimits::default(),
            &scratch,
        );
        assert_eq!(
            result.classification_of(&x.hash(), StorageRole::Object),
            Some(Classification::Deferred {
                protectors: vec![prot(ProtectorKind::FutureBatchMember, facts.members[0])],
                namespace_collision: false,
            })
        );
        // The flattened plan list alone is insufficient: the
        // anchors are not plan members, yet their references
        // protect candidates (the derive's exact partition proves
        // it; the anchor non-membership is the structural fact).
        let plan_records: Vec<Hash> = plan
            .batches
            .iter()
            .flat_map(|batch| batch.records.iter().copied())
            .collect();
        for anchor in &facts.anchors {
            assert!(
                !plan_records.contains(anchor),
                "the anchor is not a plan member — the flattened list alone misses it"
            );
        }
        // (b) The PRIOR-batch conditional premise manifests:
        // selecting the oldest member's batch, X is a current
        // candidate also covered by the newest member's PRIOR
        // audit — Mandatory with the conditional flag, never a
        // completion claim and never protection.
        let result = account(
            &mut medium,
            &facts.mk,
            oldest_batch,
            &AccountingLimits::default(),
            &scratch,
        );
        assert_eq!(
            result.classification_of(&x.hash(), StorageRole::Object),
            Some(Classification::Mandatory {
                conditional_on_prior_batches: true
            })
        );
        assert!(matches!(
            result.prior_batch_premise,
            PriorBatchPremise::ConditionalOnPriorBatches { .. }
        ));
        // (c) The below-boundary history: a genuine prior
        // checkpoint below the current range — every record below
        // the boundary walked live and conservatively protected,
        // the shared object protected by the below-boundary roots.
        drop(medium);
        cleanup(&[root, journal, scratch]);
        let (root2, mut medium2) = a3_store("t2b");
        let journal2 = a3_journal("t2b");
        let scratch2 = a3_scratch("t2b");
        let facts2 = a3_campaign(
            &mut medium2,
            "t2b",
            &journal2,
            &[
                member_step(
                    a3_member_key("t2b", 0),
                    vec![plain_object(b"a3-t2b-payload-m0")],
                ),
                member_step(
                    a3_member_key("t2b", 1),
                    vec![plain_object(b"a3-t2b-payload-m1")],
                ),
            ],
            10_000,
            true,
            None,
        );
        a3_complete(&mut medium2, &facts2.mk);
        let result2 = account(
            &mut medium2,
            &facts2.mk,
            0,
            &AccountingLimits::default(),
            &scratch2,
        );
        assert_eq!(result2.survivors.boundary, facts2.prior_mk.unwrap());
        assert!(result2.survivors.boundary_prior_checkpoint);
        assert_eq!(result2.survivors.below_boundary_revisions, 5);
        assert_eq!(
            result2.limitations.deeper_history,
            DeeperHistoryScope::BelowBoundaryWalked { revisions: 5 }
        );
        // The shared object (SK's audited unit object) is protected
        // by the below-boundary roots: the prior SK, the prior
        // member, p1, and the founding.
        assert_eq!(
            result2.classification_of(&facts2.shared, StorageRole::Object),
            Some(Classification::Deferred {
                protectors: {
                    let roots = std::collections::BTreeSet::from([
                        prot(
                            ProtectorKind::BelowBoundaryHistory,
                            facts2.prior_sk.unwrap(),
                        ),
                        prot(
                            ProtectorKind::BelowBoundaryHistory,
                            facts2.prior_m0.unwrap(),
                        ),
                        prot(ProtectorKind::BelowBoundaryHistory, facts2.p1),
                        prot(ProtectorKind::BelowBoundaryHistory, facts2.founding),
                    ]);
                    roots.into_iter().collect::<Vec<_>>()
                },
                namespace_collision: false,
            })
        );
        drop(medium2);
        cleanup(&[root2, journal2, scratch2]);
    }

    /// (A3-T3) The cross-unit sharing: multiple current-batch units
    /// reference the same object — accounted exactly once under the
    /// global-first-seen rule with every referencing unit recorded;
    //  and when a surviving anchor also references it, it becomes
    /// protected rather than eligible for physical removal.
    #[cfg(unix)]
    #[test]
    fn the_cross_unit_sharing_accounts_once() {
        let x = plain_object(b"a3-t3-shared");
        // (a) No survivor references X: Mandatory exactly once.
        let (root, mut medium) = a3_store("t3a");
        let journal = a3_journal("t3a");
        let scratch = a3_scratch("t3a");
        let facts = a3_campaign(
            &mut medium,
            "t3a",
            &journal,
            &[
                member_step(
                    a3_member_key("t3a", 0),
                    vec![x.clone(), plain_object(b"a3-t3-own-0")],
                ),
                member_step(
                    a3_member_key("t3a", 1),
                    vec![x.clone(), plain_object(b"a3-t3-own-1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        let result = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        assert_eq!(
            result
                .mandatory
                .iter()
                .filter(|entry| entry.identity == x.hash())
                .count(),
            1,
            "the shared object is accounted exactly once"
        );
        let shared_entry = result
            .shared_references
            .iter()
            .find(|entry| entry.identity == x.hash())
            .unwrap();
        // The audit units are [SK, m1, m0] — the first-seen unit is
        // m1's (unit 1), and both units are recorded.
        assert_eq!(shared_entry.role, StorageRole::Object);
        assert_eq!(shared_entry.referencing_units, vec![1, 2]);
        let first_seen = result
            .mandatory
            .iter()
            .find(|entry| entry.identity == x.hash())
            .unwrap()
            .first_seen_unit;
        assert_eq!(first_seen, 1, "the global-first-seen attribution");
        drop(medium);
        cleanup(&[root, journal, scratch]);
        // (b) A surviving anchor references X: protected.
        let (root, mut medium) = a3_store("t3b");
        let journal = a3_journal("t3b");
        let scratch = a3_scratch("t3b");
        let facts = a3_campaign(
            &mut medium,
            "t3b",
            &journal,
            &[
                member_step(
                    a3_member_key("t3b", 0),
                    vec![x.clone(), plain_object(b"a3-t3-own-0")],
                ),
                anchor_step(vec![x.clone()]),
                member_step(
                    a3_member_key("t3b", 1),
                    vec![x.clone(), plain_object(b"a3-t3-own-1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        let result = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        assert_eq!(
            result.classification_of(&x.hash(), StorageRole::Object),
            Some(Classification::Deferred {
                protectors: vec![prot(ProtectorKind::RangeAnchor, facts.anchors[0])],
                namespace_collision: false,
            })
        );
        // The shared classification persists alongside the
        // protection.
        let shared_entry = result
            .shared_references
            .iter()
            .find(|entry| entry.identity == x.hash())
            .unwrap();
        assert_eq!(shared_entry.referencing_units, vec![1, 2]);
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T4) The composite closure protection: a retiring member
    /// whose immutable audit carries closure-expanded dependencies
    /// (a composite snapshot root published above the checkpoint —
    /// a LIVE-CHAIN surviving root whose verified closure protects
    /// the shared dependency). The accounting never relies solely
    /// on direct record references.
    #[cfg(unix)]
    #[test]
    fn the_composite_closure_protection() {
        crate::composite_append::test_enable_composite_publication();
        let (root, mut medium) = a3_store("t4");
        let journal = a3_journal("t4");
        let scratch = a3_scratch("t4");
        // The shared chunk: created with the retiring member (whose
        // audit lists it), and REUSED by the surviving composite
        // snapshot's manifest above the MK.
        let shared_payload = b"a3-t4-shared-chunk-payload".to_vec();
        let shared_chunk = crate::composite_manifest::encode_chunk_envelope(
            crate::composite_manifest::TAG_WORLD,
            &shared_payload,
        );
        // The composite snapshot above the MK: the full manifest
        // shape with the WORLD slot REUSING the shared chunk.
        let mut seed = 40u8;
        let mut entries = Vec::new();
        let mut fresh_chunks = Vec::new();
        let push_singleton = |entries: &mut Vec<crate::composite_manifest::ManifestEntry>,
                              chunks: &mut Vec<StoredObject>,
                              seed: &mut u8,
                              tag: &'static str| {
            let payload = vec![*seed; 10 + *seed as usize];
            *seed = seed.wrapping_add(1);
            let object = crate::composite_manifest::encode_chunk_envelope(tag, &payload);
            entries.push(crate::composite_manifest::ManifestEntry {
                tag,
                identity: object.hash(),
                payload_length: payload.len() as u64,
                kind: crate::composite_manifest::SectionKind::Singleton,
            });
            chunks.push(object);
        };
        for tag in [
            crate::composite_manifest::TAG_HEADER,
            crate::composite_manifest::TAG_NEAR,
            crate::composite_manifest::TAG_FAR,
            crate::composite_manifest::TAG_BLOCKED,
            crate::composite_manifest::TAG_HELD,
            crate::composite_manifest::TAG_LEDGER_META,
        ] {
            push_singleton(&mut entries, &mut fresh_chunks, &mut seed, tag);
        }
        let sealed_width = 4u64 * 31;
        for _ in 0..2 {
            let payload = vec![seed; sealed_width as usize];
            seed = seed.wrapping_add(1);
            let object = crate::composite_manifest::encode_chunk_envelope(
                crate::composite_manifest::TAG_LEDGER_RECORDS,
                &payload,
            );
            entries.push(crate::composite_manifest::ManifestEntry {
                tag: crate::composite_manifest::TAG_LEDGER_RECORDS,
                identity: object.hash(),
                payload_length: sealed_width,
                kind: crate::composite_manifest::SectionKind::LedgerSealed,
            });
            fresh_chunks.push(object);
        }
        let tail = 2u64 * 31;
        let payload = vec![seed; tail as usize];
        seed = seed.wrapping_add(1);
        let object = crate::composite_manifest::encode_chunk_envelope(
            crate::composite_manifest::TAG_LEDGER_RECORDS,
            &payload,
        );
        entries.push(crate::composite_manifest::ManifestEntry {
            tag: crate::composite_manifest::TAG_LEDGER_RECORDS,
            identity: object.hash(),
            payload_length: tail,
            kind: crate::composite_manifest::SectionKind::LedgerTail,
        });
        fresh_chunks.push(object);
        entries.push(crate::composite_manifest::ManifestEntry {
            tag: crate::composite_manifest::TAG_WORLD,
            identity: shared_chunk.hash(),
            payload_length: shared_payload.len() as u64,
            kind: crate::composite_manifest::SectionKind::Singleton,
        });
        push_singleton(
            &mut entries,
            &mut fresh_chunks,
            &mut seed,
            crate::composite_manifest::TAG_DIRECTOR,
        );
        let manifest = crate::composite_manifest::CompositeManifest {
            history_kind: crate::composite_manifest::HistoryKind::Full,
            ledger_boundary: 4,
            composed_length: entries.iter().map(|entry| entry.payload_length).sum(),
            entries,
        };
        let root_object = StoredObject::new(crate::composite_manifest::encode(&manifest)).unwrap();
        let above = A3AboveComposite {
            root: root_object,
            new_chunks: fresh_chunks,
            reused_chunks: vec![shared_chunk.hash()],
        };
        // The retiring member references the shared chunk directly
        // (the chunk domain is outside the snapshot family — the
        // generic append allows it).
        let facts = a3_campaign(
            &mut medium,
            "t4",
            &journal,
            &[
                member_step(
                    a3_member_key("t4", 0),
                    vec![shared_chunk.clone(), plain_object(b"a3-t4-own-0")],
                ),
                member_step(a3_member_key("t4", 1), vec![plain_object(b"a3-t4-own-1")]),
            ],
            10_000,
            false,
            Some(&above),
        );
        a3_complete(&mut medium, &facts.mk);
        let result = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let snapshot = facts.above_mk_composite.unwrap();
        // The shared chunk — a retiring member's audited object —
        // is protected by the LIVE-CHAIN composite root's verified
        // closure (the dependency never appears in any direct
        // record reference of the survivor).
        assert_eq!(
            result.classification_of(&shared_chunk.hash(), StorageRole::Object),
            Some(Classification::Deferred {
                protectors: vec![prot(ProtectorKind::LiveChainRevision, snapshot)],
                namespace_collision: false,
            })
        );
        // The closure actually expanded: the surviving root's
        // composite closure carried more than its direct reference.
        assert!(result.survivors.composite_closure_roots >= 1);
        // The other audited objects remain Mandatory.
        for payload in [b"a3-t4-own-0".as_slice(), b"a3-t4-own-1".as_slice()] {
            assert_eq!(
                result.classification_of(&plain_object(payload).hash(), StorageRole::Object),
                Some(Classification::Mandatory {
                    conditional_on_prior_batches: false
                })
            );
        }
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T5) The physically absent historical members: a
    /// test-only partial retirement (the prior-batch members'
    /// record files directly removed — no production deletion path
    /// invoked). The A2 walker reconstructs their original facts;
    /// the accounting neither mistakes the absence for completion
    /// authority (the premise and the conditional flags persist)
    /// nor loses the legitimate surviving protections (the exact
    /// partition is unchanged).
    #[cfg(unix)]
    #[test]
    fn the_absent_prior_members_stay_conditional() {
        let x = plain_object(b"a3-t5-cross-batch-shared");
        let (root, mut medium) = a3_store("t5");
        let journal = a3_journal("t5");
        let scratch = a3_scratch("t5");
        let steps: Vec<A3Step> = (0..6usize)
            .map(|index| {
                let objects = if index == 0 || index == 5 {
                    vec![x.clone()]
                } else {
                    vec![plain_object(format!("a3-t5-own-{index}").as_bytes())]
                };
                member_step(a3_member_key("t5", index), objects)
            })
            .collect();
        let facts = a3_campaign(&mut medium, "t5", &journal, &steps, 500, false, None);
        a3_complete(&mut medium, &facts.mk);
        let (_plan_record, plan) = crate::reclaim::find_installed_plan(&mut medium, &facts.mk)
            .unwrap()
            .unwrap();
        assert!(plan.batches.len() >= 2, "the multi-batch fixture");
        let oldest_batch = plan.batches.last().unwrap().index;
        // The test-only partial retirement: every PRIOR-batch
        // member's record file removed (including SK when it lands
        // in a prior batch).
        let prior_records: Vec<Hash> = plan
            .batches
            .iter()
            .filter(|batch| batch.index < oldest_batch)
            .flat_map(|batch| batch.records.iter().copied())
            .collect();
        assert!(!prior_records.is_empty());
        for record in &prior_records {
            std::fs::remove_file(medium.object_path_pub(record)).unwrap();
        }
        // The accounting succeeds over the authenticated evidence:
        // the audit is the authority, the witness reconstructs the
        // members, and the begin authority is recovery-safe.
        let result = account(
            &mut medium,
            &facts.mk,
            oldest_batch,
            &AccountingLimits::default(),
            &scratch,
        );
        let (expected_mandatory, expected_deferred, expected_shared) =
            derive_expected(&facts, &plan, oldest_batch);
        assert_eq!(
            result.mandatory, expected_mandatory,
            "the absent prior members change nothing in the partition"
        );
        assert_eq!(result.deferred, expected_deferred);
        assert_eq!(result.shared_references, expected_shared);
        // The absence is NOT completion authority: the premise
        // still names the prior batches, and the cross-batch
        // shared object still carries the conditional flag.
        assert!(matches!(
            result.prior_batch_premise,
            PriorBatchPremise::ConditionalOnPriorBatches { .. }
        ));
        assert_eq!(
            result.classification_of(&x.hash(), StorageRole::Object),
            Some(Classification::Mandatory {
                conditional_on_prior_batches: true
            })
        );
        // The legitimate surviving protections are intact: the
        // newest batch (where SK's unit is current) still defers
        // the shared object under the founding boundary's
        // protection — the absent prior members never became
        // protection and never removed one.
        let newest_result = account(
            &mut medium,
            &facts.mk,
            plan.batches[0].index,
            &AccountingLimits::default(),
            &scratch,
        );
        assert_eq!(
            newest_result.classification_of(&facts.shared, StorageRole::Object),
            Some(Classification::Deferred {
                protectors: vec![
                    prot(ProtectorKind::RangeAnchor, facts.p1),
                    prot(ProtectorKind::RangeBoundary, facts.founding),
                ],
                namespace_collision: false,
            })
        );
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T6) The reintroduced record-preimage bytes: the retiring
    /// member's canonical record bytes reintroduced as an ordinary
    /// content object referenced by a retained anchor. The logical
    /// retirement membership is unchanged (the record identity
    /// remains a candidate), the live content protection is
    /// preserved (the bytes are conservatively protected), the
    /// physical presence never resurrects the record's historical
    /// authority, and the namespace conflict is represented safely
    /// (the collision classification, never contradictory physical
    /// obligations).
    #[cfg(unix)]
    #[test]
    fn the_reintroduced_preimage_bytes_collision() {
        let (root, mut medium) = a3_store("t6");
        let journal = a3_journal("t6");
        let scratch = a3_scratch("t6");
        let facts = a3_campaign(
            &mut medium,
            "t6",
            &journal,
            &[
                member_step(
                    a3_member_key("t6", 0),
                    vec![plain_object(b"a3-t6-payload-m0")],
                ),
                A3Step::Reintroduce,
                member_step(
                    a3_member_key("t6", 1),
                    vec![plain_object(b"a3-t6-payload-m1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        let result = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let m0 = facts.members[0];
        let anchor = facts.anchors[0];
        // The logical retirement membership is unchanged: the
        // member's record identity remains an audited candidate
        // (now Deferred — conservatively protected).
        assert_eq!(
            result.classification_of(&m0, StorageRole::Record),
            Some(Classification::Deferred {
                protectors: vec![prot(ProtectorKind::RangeAnchor, anchor)],
                namespace_collision: true,
            })
        );
        // The collision is represented explicitly with the
        // conservatively-protected classification.
        assert_eq!(result.namespace_collisions.len(), 1);
        assert_eq!(result.namespace_collisions[0].identity, m0);
        assert_eq!(
            result.namespace_collisions[0].candidate_role,
            StorageRole::Record
        );
        assert_eq!(result.namespace_collisions[0].first_seen_unit, 2);
        assert_eq!(
            result.namespace_collisions[0].protectors,
            vec![prot(ProtectorKind::RangeAnchor, anchor)]
        );
        assert_eq!(
            result.namespace_collisions[0].classification,
            CollisionClassification::ConservativelyProtected
        );
        assert_eq!(result.counts.namespace_collisions, 1);
        // The wrong-role lookup of the same hash is NOT the
        // collision (the object-role identity is not a candidate).
        assert_eq!(result.classification_of(&m0, StorageRole::Object), None);
        // The other members' facts are untouched.
        assert_eq!(
            result.classification_of(&facts.members[1], StorageRole::Record),
            Some(Classification::Mandatory {
                conditional_on_prior_batches: false
            })
        );
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T7) The global-first-seen determinism: repeated
    /// identities across units and across record/object roles; a
    /// different iteration order (the forced-small spill
    /// thresholds changing the run boundaries) never silently
    /// changes the canonical result.
    #[cfg(unix)]
    #[test]
    fn the_global_first_seen_determinism() {
        let x = plain_object(b"a3-t7-cross-unit-x");
        let y = plain_object(b"a3-t7-cross-unit-y");
        let (root, mut medium) = a3_store("t7");
        let journal = a3_journal("t7");
        let scratch = a3_scratch("t7");
        let steps: Vec<A3Step> = vec![
            member_step(a3_member_key("t7", 0), vec![x.clone(), y.clone()]),
            member_step(a3_member_key("t7", 1), vec![y.clone()]),
            member_step(
                a3_member_key("t7", 2),
                vec![x.clone(), plain_object(b"a3-t7-own-2")],
            ),
            member_step(a3_member_key("t7", 3), vec![y.clone(), x.clone()]),
        ];
        let facts = a3_campaign(&mut medium, "t7", &journal, &steps, 10_000, false, None);
        a3_complete(&mut medium, &facts.mk);
        let wide = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let tiny = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits {
                run_entry_buffer: 4,
                max_runs: 64,
                max_scratch_bytes: 1 << 20,
                ..AccountingLimits::default()
            },
            &scratch,
        );
        assert!(tiny.scratch.runs_written >= 3, "the tiny limits spill");
        // The canonical result is identical across the run
        // boundaries (every exact field except the scratch report,
        // which legitimately reflects the different thresholds).
        assert_eq!(wide.mandatory, tiny.mandatory);
        assert_eq!(wide.deferred, tiny.deferred);
        assert_eq!(wide.shared_references, tiny.shared_references);
        assert_eq!(wide.namespace_collisions, tiny.namespace_collisions);
        assert_eq!(wide.counts, tiny.counts);
        assert_eq!(wide.prior_batch_premise, tiny.prior_batch_premise);
        assert_eq!(wide.survivors, tiny.survivors);
        // The first-seen attribution is the earliest unit in the
        // audit's persisted order: the units are [SK, m3, m2, m1,
        // m0], so x's first unit is m3's (unit 1) and y's is m1's
        // (unit 3).
        let x_entry = tiny
            .mandatory
            .iter()
            .find(|entry| entry.identity == x.hash())
            .unwrap();
        assert_eq!(x_entry.first_seen_unit, 1);
        let y_entry = tiny
            .mandatory
            .iter()
            .find(|entry| entry.identity == y.hash())
            .unwrap();
        assert_eq!(y_entry.first_seen_unit, 1);
        assert_eq!(
            tiny.shared_references
                .iter()
                .find(|entry| entry.identity == x.hash())
                .unwrap()
                .referencing_units,
            vec![1, 2, 4]
        );
        assert_eq!(
            tiny.shared_references
                .iter()
                .find(|entry| entry.identity == y.hash())
                .unwrap()
                .referencing_units,
            vec![1, 3, 4]
        );
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T8) The exact equality adversaries: equal-cardinality
    /// but different-member sets, and the duplicate, omitted,
    /// substituted, and extra identity cases — every invalid
    /// comparison fails even when the counts agree; the result
    /// lists carry no duplicates and the counts are coherent.
    #[cfg(unix)]
    #[test]
    fn the_exact_equality_adversaries() {
        let (root, mut medium) = a3_store("t8");
        let journal = a3_journal("t8");
        let scratch = a3_scratch("t8");
        let facts = a3_campaign(
            &mut medium,
            "t8",
            &journal,
            &[
                member_step(
                    a3_member_key("t8", 0),
                    vec![plain_object(b"a3-t8-payload-m0")],
                ),
                member_step(
                    a3_member_key("t8", 1),
                    vec![plain_object(b"a3-t8-payload-m1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        let result = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let true_mandatory = result.mandatory.clone();
        let unused = crate::hash::hash_object(b"a3-t8-unused-identity");
        // (a) Equal cardinality, different member.
        let mut adversarial = true_mandatory.clone();
        adversarial[0].identity = unused;
        assert_ne!(result.mandatory, adversarial);
        // (b) Duplicate.
        let mut adversarial = true_mandatory.clone();
        adversarial.push(adversarial[0].clone());
        assert_ne!(result.mandatory, adversarial);
        // (c) Omitted.
        let mut adversarial = true_mandatory.clone();
        adversarial.pop();
        assert_ne!(result.mandatory, adversarial);
        // (d) Substituted role (the deterministic flip — whatever
        // the entry's true role, the adversary carries the other).
        let mut adversarial = true_mandatory.clone();
        adversarial[1].role = if adversarial[1].role == StorageRole::Record {
            StorageRole::Object
        } else {
            StorageRole::Record
        };
        assert_ne!(result.mandatory, adversarial);
        // (e) Misclassified (a deferred identity moved to
        // mandatory).
        let mut adversarial = true_mandatory.clone();
        adversarial[2] = MandatoryIdentity {
            identity: facts.shared,
            role: StorageRole::Object,
            first_seen_unit: 0,
            conditional_on_prior_batches: false,
        };
        assert_ne!(result.mandatory, adversarial);
        // The result itself carries no duplicates and coherent
        // counts.
        let mut seen = std::collections::BTreeSet::new();
        for entry in result
            .mandatory
            .iter()
            .map(|e| (e.identity, e.role))
            .chain(result.deferred.iter().map(|e| (e.identity, e.role)))
        {
            assert!(seen.insert(entry), "no duplicate (identity, role) pair");
        }
        assert_eq!(seen.len() as u64, result.counts.candidate_identities);
        assert_eq!(result.counts.mandatory, result.mandatory.len() as u64);
        assert_eq!(result.counts.deferred, result.deferred.len() as u64);
        assert_eq!(
            result.counts.shared_references,
            result.shared_references.len() as u64
        );
        assert_eq!(
            result.counts.namespace_collisions,
            result.namespace_collisions.len() as u64
        );
        // The near-miss membership lookups fail: the wrong role and
        // the nearby identity.
        assert_eq!(result.classification_of(&unused, StorageRole::Record), None);
        assert_eq!(
            result.classification_of(&facts.members[0], StorageRole::Object),
            None,
            "the wrong role never matches"
        );
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T9, the campaign half) The scratch spill boundaries over
    /// a genuine campaign: the forced-small thresholds produce a
    /// multi-run merge, the duplicates crossing run boundaries
    /// account once, and the output is exact across the boundaries.
    #[cfg(unix)]
    #[test]
    fn the_scratch_spill_boundaries_are_exact() {
        let x = plain_object(b"a3-t9-cross-unit-x");
        let (root, mut medium) = a3_store("t9");
        let journal = a3_journal("t9");
        let scratch = a3_scratch("t9");
        let steps: Vec<A3Step> = vec![
            member_step(a3_member_key("t9", 0), vec![x.clone()]),
            member_step(a3_member_key("t9", 1), vec![x.clone()]),
            member_step(a3_member_key("t9", 2), vec![x.clone()]),
        ];
        let facts = a3_campaign(&mut medium, "t9", &journal, &steps, 10_000, false, None);
        a3_complete(&mut medium, &facts.mk);
        let wide = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let tiny = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits {
                run_entry_buffer: 3,
                max_runs: 64,
                max_scratch_bytes: 1 << 20,
                ..AccountingLimits::default()
            },
            &scratch,
        );
        assert!(tiny.scratch.runs_written >= 4, "the tiny thresholds spill");
        assert_eq!(wide.mandatory, tiny.mandatory);
        assert_eq!(wide.deferred, tiny.deferred);
        assert_eq!(wide.shared_references, tiny.shared_references);
        // The shared object referenced by units whose entries land
        // in different runs still accounts once with every unit.
        let shared_entry = tiny
            .shared_references
            .iter()
            .find(|entry| entry.identity == x.hash())
            .unwrap();
        assert_eq!(shared_entry.referencing_units, vec![1, 2, 3]);
        // No timing-based assertion anywhere: the exactness is the
        // proof.
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T10) The evidence integrity refusals: the degraded
    /// witness, the missing audit, the unknown batch, the
    /// uninstalled MK, the absent begin, the corrupted closure
    /// evidence, and the broken survivor chain — every invalid
    /// state refuses closed. (The audit-binding and audit-order
    /// adversaries are enforced by the CP2 begin authority the
    /// accounting calls FIRST — the propagated refusal is the
    /// coverage.)
    #[cfg(unix)]
    #[test]
    fn the_evidence_integrity_refusals() {
        // (a) The degraded witness: the corrupted manifest object
        // refuses at the A2 walk's Phase 0 gate.
        {
            let (root, mut medium) = a3_store("t10a");
            let journal = a3_journal("t10a");
            let scratch = a3_scratch("t10a");
            let facts = a3_campaign(
                &mut medium,
                "t10a",
                &journal,
                &[member_step(
                    a3_member_key("t10a", 0),
                    vec![plain_object(b"a3-t10a-payload-m0")],
                )],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            let verified = match crate::ancestry_witness::verify_installed_ancestry_witness(
                &mut medium,
                &facts.mk,
            )
            .unwrap()
            {
                crate::ancestry_witness::InstalledWitnessVerification::Verified {
                    manifest_record,
                    ..
                } => manifest_record,
                other => panic!("the intact witness verifies: {other:?}"),
            };
            let manifest_object = medium.read_record(&verified).unwrap().content_refs[0];
            // The corruption lands behind the open medium's back
            // (the reads go to disk per call — no reopen, whose own
            // cold-walk validation would refuse first).
            std::fs::write(
                medium_object_path(&root, &manifest_object),
                b"corrupted-manifest-bytes",
            )
            .unwrap();
            let error = account_batch_retirement(
                &mut medium,
                &facts.mk,
                0,
                &AccountingLimits::default(),
                &scratch,
            )
            .unwrap_err();
            // The corrupted witness object refuses closed at the
            // walk's Phase 0 gate — the typed integrity error
            // propagates from the verifier's own hash-verified read
            // (the degraded-outcome classification is the other
            // legitimate arm; both are fail-closed, never a walk).
            assert!(
                matches!(
                    error,
                    MediumError::HashMismatch { .. }
                        | MediumError::Chain {
                            rule: "history/witness-not-verified"
                        }
                ),
                "the degraded witness refuses at the walk's gate: {error:?}"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (b) The missing audit object: the begin authority's
        // audit verification refuses (the typed missing-object
        // classification).
        {
            let (root, mut medium) = a3_store("t10b");
            let journal = a3_journal("t10b");
            let scratch = a3_scratch("t10b");
            let facts = a3_campaign(
                &mut medium,
                "t10b",
                &journal,
                &[member_step(
                    a3_member_key("t10b", 0),
                    vec![plain_object(b"a3-t10b-payload-m0")],
                )],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            let (audit_record, _ledger) =
                crate::reclaim::find_installed_v2_audit(&mut medium, &facts.mk, 0)
                    .unwrap()
                    .unwrap();
            let audit_object = medium.read_record(&audit_record).unwrap().content_refs[0];
            std::fs::remove_file(medium_object_path(&root, &audit_object)).unwrap();
            let error = account_batch_retirement(
                &mut medium,
                &facts.mk,
                0,
                &AccountingLimits::default(),
                &scratch,
            )
            .unwrap_err();
            assert!(
                matches!(error, MediumError::MissingObject { .. }),
                "the missing audit refuses closed: {error:?}"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (c) The unknown batch.
        {
            let (root, mut medium) = a3_store("t10c");
            let journal = a3_journal("t10c");
            let scratch = a3_scratch("t10c");
            let facts = a3_campaign(
                &mut medium,
                "t10c",
                &journal,
                &[member_step(
                    a3_member_key("t10c", 0),
                    vec![plain_object(b"a3-t10c-payload-m0")],
                )],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            let error = account_batch_retirement(
                &mut medium,
                &facts.mk,
                99,
                &AccountingLimits::default(),
                &scratch,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "accounting/unknown-batch"
                    }
                ),
                "the unknown batch refuses typed: {error:?}"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (d) The uninstalled MK: the begin authority refuses (the
        // MK authentication runs inside the accepted verifier the
        // accounting calls first).
        {
            let (root, mut medium) = a3_store("t10d");
            let journal = a3_journal("t10d");
            let scratch = a3_scratch("t10d");
            let facts = a3_campaign(
                &mut medium,
                "t10d",
                &journal,
                &[member_step(
                    a3_member_key("t10d", 0),
                    vec![plain_object(b"a3-t10d-payload-m0")],
                )],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            let bogus = crate::hash::hash_object(b"a3-t10d-bogus-mk");
            assert!(
                account_batch_retirement(
                    &mut medium,
                    &bogus,
                    0,
                    &AccountingLimits::default(),
                    &scratch,
                )
                .is_err(),
                "the uninstalled MK refuses closed"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (e) The absent begin: the campaign without the begin
        // installation refuses at the begin authority.
        {
            let (root, mut medium) = a3_store("t10e");
            let journal = a3_journal("t10e");
            let scratch = a3_scratch("t10e");
            let facts = a3_campaign(
                &mut medium,
                "t10e",
                &journal,
                &[member_step(
                    a3_member_key("t10e", 0),
                    vec![plain_object(b"a3-t10e-payload-m0")],
                )],
                10_000,
                false,
                None,
            );
            // No begin, no witness.
            assert!(
                account_batch_retirement(
                    &mut medium,
                    &facts.mk,
                    0,
                    &AccountingLimits::default(),
                    &scratch,
                )
                .is_err(),
                "the absent begin refuses closed"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (f) The corrupted closure evidence: a surviving composite
        // root's fresh chunk (referenced only through its manifest)
        // removed — the accounting's own closure verification
        // refuses.
        {
            crate::composite_append::test_enable_composite_publication();
            let (root, mut medium) = a3_store("t10f");
            let journal = a3_journal("t10f");
            let scratch = a3_scratch("t10f");
            let shared_payload = b"a3-t10f-shared-chunk".to_vec();
            let shared_chunk = crate::composite_manifest::encode_chunk_envelope(
                crate::composite_manifest::TAG_WORLD,
                &shared_payload,
            );
            let mut seed = 90u8;
            let mut entries = Vec::new();
            let mut fresh_chunks = Vec::new();
            let push_singleton = |entries: &mut Vec<crate::composite_manifest::ManifestEntry>,
                                  chunks: &mut Vec<StoredObject>,
                                  seed: &mut u8,
                                  tag: &'static str| {
                let payload = vec![*seed; 10 + *seed as usize];
                *seed = seed.wrapping_add(1);
                let object = crate::composite_manifest::encode_chunk_envelope(tag, &payload);
                entries.push(crate::composite_manifest::ManifestEntry {
                    tag,
                    identity: object.hash(),
                    payload_length: payload.len() as u64,
                    kind: crate::composite_manifest::SectionKind::Singleton,
                });
                chunks.push(object);
            };
            for tag in [
                crate::composite_manifest::TAG_HEADER,
                crate::composite_manifest::TAG_NEAR,
                crate::composite_manifest::TAG_FAR,
                crate::composite_manifest::TAG_BLOCKED,
                crate::composite_manifest::TAG_HELD,
                crate::composite_manifest::TAG_LEDGER_META,
            ] {
                push_singleton(&mut entries, &mut fresh_chunks, &mut seed, tag);
            }
            let sealed_width = 4u64 * 31;
            for _ in 0..2 {
                let payload = vec![seed; sealed_width as usize];
                seed = seed.wrapping_add(1);
                let object = crate::composite_manifest::encode_chunk_envelope(
                    crate::composite_manifest::TAG_LEDGER_RECORDS,
                    &payload,
                );
                entries.push(crate::composite_manifest::ManifestEntry {
                    tag: crate::composite_manifest::TAG_LEDGER_RECORDS,
                    identity: object.hash(),
                    payload_length: sealed_width,
                    kind: crate::composite_manifest::SectionKind::LedgerSealed,
                });
                fresh_chunks.push(object);
            }
            let tail = 2u64 * 31;
            let payload = vec![seed; tail as usize];
            let object = crate::composite_manifest::encode_chunk_envelope(
                crate::composite_manifest::TAG_LEDGER_RECORDS,
                &payload,
            );
            entries.push(crate::composite_manifest::ManifestEntry {
                tag: crate::composite_manifest::TAG_LEDGER_RECORDS,
                identity: object.hash(),
                payload_length: tail,
                kind: crate::composite_manifest::SectionKind::LedgerTail,
            });
            fresh_chunks.push(object);
            entries.push(crate::composite_manifest::ManifestEntry {
                tag: crate::composite_manifest::TAG_WORLD,
                identity: shared_chunk.hash(),
                payload_length: shared_payload.len() as u64,
                kind: crate::composite_manifest::SectionKind::Singleton,
            });
            push_singleton(
                &mut entries,
                &mut fresh_chunks,
                &mut seed,
                crate::composite_manifest::TAG_DIRECTOR,
            );
            let manifest = crate::composite_manifest::CompositeManifest {
                history_kind: crate::composite_manifest::HistoryKind::Full,
                ledger_boundary: 4,
                composed_length: entries.iter().map(|entry| entry.payload_length).sum(),
                entries,
            };
            let root_object =
                StoredObject::new(crate::composite_manifest::encode(&manifest)).unwrap();
            let above = A3AboveComposite {
                root: root_object,
                new_chunks: fresh_chunks.clone(),
                reused_chunks: vec![shared_chunk.hash()],
            };
            let facts = a3_campaign(
                &mut medium,
                "t10f",
                &journal,
                &[member_step(
                    a3_member_key("t10f", 0),
                    vec![shared_chunk.clone()],
                )],
                10_000,
                false,
                Some(&above),
            );
            a3_complete(&mut medium, &facts.mk);
            // The HEADER chunk — referenced only through the
            // manifest — removed.
            std::fs::remove_file(medium_object_path(&root, &fresh_chunks[0].hash())).unwrap();
            let error = account_batch_retirement(
                &mut medium,
                &facts.mk,
                0,
                &AccountingLimits::default(),
                &scratch,
            )
            .unwrap_err();
            assert!(
                matches!(error, MediumError::MissingObject { .. }),
                "the corrupted closure evidence refuses closed: {error:?}"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (g) The broken survivor chain: a retained anchor's record
        // removed — the walk's live anchor read refuses.
        {
            let (root, mut medium) = a3_store("t10g");
            let journal = a3_journal("t10g");
            let scratch = a3_scratch("t10g");
            let facts = a3_campaign(
                &mut medium,
                "t10g",
                &journal,
                &[
                    member_step(
                        a3_member_key("t10g", 0),
                        vec![plain_object(b"a3-t10g-payload-m0")],
                    ),
                    anchor_step(vec![plain_object(b"a3-t10g-anchor-object")]),
                    member_step(
                        a3_member_key("t10g", 1),
                        vec![plain_object(b"a3-t10g-payload-m1")],
                    ),
                ],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            std::fs::remove_file(medium_object_path(&root, &facts.anchors[0])).unwrap();
            assert!(
                account_batch_retirement(
                    &mut medium,
                    &facts.mk,
                    0,
                    &AccountingLimits::default(),
                    &scratch,
                )
                .is_err(),
                "the broken survivor chain refuses closed"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
    }

    /// The local store's content-addressed object path (the test
    /// seam — the same layout the medium itself uses).
    #[cfg(unix)]
    fn medium_object_path(root: &std::path::Path, hash: &Hash) -> std::path::PathBuf {
        root.join("objects").join(hash.hex())
    }

    /// (A3-T11) The provider consistency limitations: the immutable
    /// historical derivation versus the changing physical storage.
    /// An unchanged HEAD alone does not authorize a live-state
    /// certification: the result carries the explicit
    /// no-coordination limitation, a physical change outside the
    /// authenticated evidence leaves the audit-derived partition
    /// unchanged (the audit is the authority), and a physical
    /// change INSIDE the evidence chain refuses closed.
    #[cfg(unix)]
    #[test]
    fn the_unchanged_head_is_not_live_certification() {
        let (root, mut medium) = a3_store("t11");
        let journal = a3_journal("t11");
        let scratch = a3_scratch("t11");
        // A retiring member with TWO content objects: the one that
        // sorts SECOND is read by no verification layer (only first
        // references are peeked on the descents, and the member is
        // not a survivor — the audit is the authority).
        let first = plain_object(b"a3-t11-first-object");
        let second = plain_object(b"a3-t11-second-object");
        let (kept, removed) = if first.hash() < second.hash() {
            (first.clone(), second.hash())
        } else {
            (second.clone(), first.hash())
        };
        let facts = a3_campaign(
            &mut medium,
            "t11",
            &journal,
            &[
                member_step(a3_member_key("t11", 0), vec![first, second]),
                member_step(
                    a3_member_key("t11", 1),
                    vec![plain_object(b"a3-t11-payload-m1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        let before = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let head_before = medium.head().unwrap();
        // The result never claims a live-state certification.
        assert_eq!(
            before.limitations.live_coordination,
            LiveCoordination::OrdinaryReadsWithoutCoordinationBoundary
        );
        // The physical change with the unchanged HEAD: the
        // non-first audited object file removed.
        std::fs::remove_file(medium_object_path(&root, &removed)).unwrap();
        let head_after = medium.head().unwrap();
        assert_eq!(head_before, head_after, "the HEAD never moved");
        // The accounting re-derives over the immutable evidence:
        // the audit still lists the physically-absent object as a
        // Mandatory candidate (the audit is the authority, never
        // the physical observation), and NO live-state
        // certification is claimed.
        let after = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        assert_eq!(after.mandatory, before.mandatory);
        assert_eq!(after.deferred, before.deferred);
        assert!(after.contains(&removed, StorageRole::Object));
        assert_eq!(
            after.limitations.live_coordination,
            LiveCoordination::OrdinaryReadsWithoutCoordinationBoundary
        );
        // A physical change INSIDE the evidence chain (a record's
        // FIRST content object — read by the begin authority's
        // descent) refuses closed: the unchanged HEAD authorizes
        // nothing.
        std::fs::remove_file(medium_object_path(&root, &kept.hash())).unwrap();
        assert_eq!(medium.head().unwrap(), head_before);
        assert!(
            account_batch_retirement(
                &mut medium,
                &facts.mk,
                0,
                &AccountingLimits::default(),
                &scratch,
            )
            .is_err(),
            "the degraded evidence chain refuses closed under the unchanged HEAD"
        );
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T12) The read-only preservation: no canonical appends,
    /// no HEAD mutation, no record or object deletion, no journal
    /// mutation, no completion marker publication, no provider
    /// authority changes, no production witness publisher
    /// activation — the store and journal file inventories are
    /// byte-for-byte unchanged and the scratch is cleaned.
    #[cfg(unix)]
    #[test]
    fn the_read_only_preservation() {
        let (root, mut medium) = a3_store("t12");
        let journal = a3_journal("t12");
        let scratch = a3_scratch("t12");
        let facts = a3_campaign(
            &mut medium,
            "t12",
            &journal,
            &[
                member_step(
                    a3_member_key("t12", 0),
                    vec![plain_object(b"a3-t12-payload-m0")],
                ),
                member_step(
                    a3_member_key("t12", 1),
                    vec![plain_object(b"a3-t12-payload-m1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        let head_before = medium.head().unwrap();
        let store_before = inventory(&root);
        let journal_before = inventory(&journal);
        let _ = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        assert_eq!(medium.head().unwrap(), head_before, "the HEAD never moved");
        assert_eq!(inventory(&root), store_before, "the store is unchanged");
        assert_eq!(
            inventory(&journal),
            journal_before,
            "the journal is unchanged"
        );
        // The scratch directory is cleaned (no residual
        // computation state).
        let residual: Vec<_> = std::fs::read_dir(&scratch).unwrap().collect();
        assert!(residual.is_empty(), "the scratch directory was removed");
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-T13) The cross-platform parity: the genuine SQLite
    /// evidence on every platform — the exact single-batch
    /// partition and the multi-batch conditional premise over the
    /// transactional provider.
    #[test]
    fn the_sqlite_twin_exact_partition() {
        let (path, mut medium) = a3_sqlite_store("t13");
        let journal = a3_journal("t13");
        let scratch = a3_scratch("t13");
        let facts = a3_campaign(
            &mut medium,
            "t13",
            &journal,
            &[
                member_step(
                    a3_member_key("t13", 0),
                    vec![plain_object(b"a3-t13-payload-m0")],
                ),
                member_step(
                    a3_member_key("t13", 1),
                    vec![plain_object(b"a3-t13-payload-m1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        let result = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let (_plan_record, plan) = crate::reclaim::find_installed_plan(&mut medium, &facts.mk)
            .unwrap()
            .unwrap();
        assert_eq!(plan.batches.len(), 1);
        let (expected_mandatory, expected_deferred, expected_shared) =
            derive_expected(&facts, &plan, 0);
        assert_eq!(result.mandatory, expected_mandatory);
        assert_eq!(result.deferred, expected_deferred);
        assert_eq!(result.shared_references, expected_shared);
        assert_eq!(result.prior_batch_premise, PriorBatchPremise::FirstBatch);
        drop(medium);
        cleanup(&[journal, scratch]);
        // The multi-batch conditional arm.
        let (path2, mut medium2) = a3_sqlite_store("t13b");
        let journal2 = a3_journal("t13b");
        let scratch2 = a3_scratch("t13b");
        let x = plain_object(b"a3-t13-cross-batch-shared");
        let steps: Vec<A3Step> = (0..6usize)
            .map(|index| {
                let objects = if index == 0 || index == 5 {
                    vec![x.clone()]
                } else {
                    vec![plain_object(format!("a3-t13-own-{index}").as_bytes())]
                };
                member_step(a3_member_key("t13b", index), objects)
            })
            .collect();
        let facts2 = a3_campaign(&mut medium2, "t13b", &journal2, &steps, 500, false, None);
        a3_complete(&mut medium2, &facts2.mk);
        let (_plan_record, plan2) = crate::reclaim::find_installed_plan(&mut medium2, &facts2.mk)
            .unwrap()
            .unwrap();
        assert!(plan2.batches.len() >= 2);
        let oldest = plan2.batches.last().unwrap().index;
        let result2 = account(
            &mut medium2,
            &facts2.mk,
            oldest,
            &AccountingLimits::default(),
            &scratch2,
        );
        assert!(matches!(
            result2.prior_batch_premise,
            PriorBatchPremise::ConditionalOnPriorBatches { .. }
        ));
        assert_eq!(
            result2.classification_of(&x.hash(), StorageRole::Object),
            Some(Classification::Mandatory {
                conditional_on_prior_batches: true
            })
        );
        drop(medium2);
        cleanup(&[journal2, scratch2]);
        a3_sqlite_cleanup(&path);
        a3_sqlite_cleanup(&path2);
    }

    /// (A3-T14) The large logical identity universe: a bounded but
    /// substantial candidate universe with repeated references and
    /// protected roots — the forced-small thresholds stream through
    /// many runs, the exact partition matches the independently
    /// constructed expectation AND the wide-threshold run, and the
    /// computation is a constant number of chain walks (never a
    /// per-identity walk).
    #[cfg(unix)]
    #[test]
    fn the_large_identity_universe_streams() {
        let (root, mut medium) = a3_store("t14");
        let journal = a3_journal("t14");
        let scratch = a3_scratch("t14");
        let steps: Vec<A3Step> = (0..40usize)
            .map(|index| {
                member_step(
                    a3_member_key("t14", index),
                    vec![plain_object(format!("a3-t14-own-{index}").as_bytes())],
                )
            })
            .collect();
        let facts = a3_campaign(&mut medium, "t14", &journal, &steps, 10_000, false, None);
        a3_complete(&mut medium, &facts.mk);
        let (_plan_record, plan) = crate::reclaim::find_installed_plan(&mut medium, &facts.mk)
            .unwrap()
            .unwrap();
        let wide = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        );
        let tiny = account(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits {
                run_entry_buffer: 8,
                max_runs: 64,
                max_scratch_bytes: 1 << 20,
                ..AccountingLimits::default()
            },
            &scratch,
        );
        let (expected_mandatory, expected_deferred, expected_shared) =
            derive_expected(&facts, &plan, 0);
        assert_eq!(tiny.mandatory, expected_mandatory);
        assert_eq!(tiny.deferred, expected_deferred);
        assert_eq!(tiny.shared_references, expected_shared);
        assert_eq!(wide.mandatory, tiny.mandatory);
        assert_eq!(wide.deferred, tiny.deferred);
        assert_eq!(wide.counts, tiny.counts);
        // The universe is substantial and the streaming is real:
        // many runs, every candidate accounted.
        assert_eq!(tiny.counts.audit_units, 41);
        assert_eq!(
            tiny.counts.candidate_identities,
            41 + 41,
            "the 41 records and the 41 objects (the shared object deferred)"
        );
        assert!(
            tiny.scratch.runs_written >= 10,
            "the tiny thresholds stream"
        );
        assert_eq!(tiny.counts.deferred, 1);
        assert_eq!(tiny.counts.mandatory, 81);
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }

    /// (A3-F1) The output ceilings: deliberately small
    /// materialized-result ceilings refuse typed (never a
    /// truncated or weakened exact result), and moderately sized
    /// ceilings that fit the universe stream the ordinary
    /// successful accounting with the exact partition.
    #[cfg(unix)]
    #[test]
    fn the_output_ceilings_refuse_and_stream() {
        // (a) The mandatory ceiling: the fixture carries five
        // Mandatory identities; a ceiling of four refuses.
        {
            let (root, mut medium) = a3_store("f1a");
            let journal = a3_journal("f1a");
            let scratch = a3_scratch("f1a");
            let facts = a3_campaign(
                &mut medium,
                "f1a",
                &journal,
                &[
                    member_step(
                        a3_member_key("f1a", 0),
                        vec![plain_object(b"a3-f1a-payload-m0")],
                    ),
                    member_step(
                        a3_member_key("f1a", 1),
                        vec![plain_object(b"a3-f1a-payload-m1")],
                    ),
                ],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            let limits = AccountingLimits {
                max_mandatory_identities: 4,
                ..AccountingLimits::default()
            };
            let error = match account_batch_retirement(&mut medium, &facts.mk, 0, &limits, &scratch)
            {
                Ok(_) => panic!("the mandatory ceiling must refuse"),
                Err(error) => error,
            };
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "accounting/mandatory-limit"
                    }
                ),
                "the mandatory ceiling refuses typed: {error:?}"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (b) The protector ceiling: the shared object carries two
        // protectors (p1 and the founding).
        {
            let (root, mut medium) = a3_store("f1b");
            let journal = a3_journal("f1b");
            let scratch = a3_scratch("f1b");
            let facts = a3_campaign(
                &mut medium,
                "f1b",
                &journal,
                &[
                    member_step(
                        a3_member_key("f1b", 0),
                        vec![plain_object(b"a3-f1b-payload-m0")],
                    ),
                    member_step(
                        a3_member_key("f1b", 1),
                        vec![plain_object(b"a3-f1b-payload-m1")],
                    ),
                ],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            let limits = AccountingLimits {
                max_protectors_per_deferred: 1,
                ..AccountingLimits::default()
            };
            let error = match account_batch_retirement(&mut medium, &facts.mk, 0, &limits, &scratch)
            {
                Ok(_) => panic!("the protector ceiling must refuse"),
                Err(error) => error,
            };
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "accounting/protector-limit"
                    }
                ),
                "the protector ceiling refuses typed: {error:?}"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (c) The deferred ceiling: a fixture with TWO deferred
        // identities (the shared object and an anchor-referenced
        // object), the ceiling of one.
        {
            let (root, mut medium) = a3_store("f1c");
            let journal = a3_journal("f1c");
            let scratch = a3_scratch("f1c");
            let x = plain_object(b"a3-f1c-anchor-shared");
            let facts = a3_campaign(
                &mut medium,
                "f1c",
                &journal,
                &[
                    member_step(a3_member_key("f1c", 0), vec![x.clone()]),
                    anchor_step(vec![x.clone()]),
                    member_step(
                        a3_member_key("f1c", 1),
                        vec![plain_object(b"a3-f1c-payload-m1")],
                    ),
                ],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            let limits = AccountingLimits {
                max_deferred_identities: 1,
                ..AccountingLimits::default()
            };
            let error = match account_batch_retirement(&mut medium, &facts.mk, 0, &limits, &scratch)
            {
                Ok(_) => panic!("the deferred ceiling must refuse"),
                Err(error) => error,
            };
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "accounting/deferred-limit"
                    }
                ),
                "the deferred ceiling refuses typed: {error:?}"
            );
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
        // (d) The ordinary successful streaming under moderately
        // small ceilings that fit the universe: the exact
        // partition, the full survivor attribution, and no
        // truncation.
        {
            let (root, mut medium) = a3_store("f1d");
            let journal = a3_journal("f1d");
            let scratch = a3_scratch("f1d");
            let facts = a3_campaign(
                &mut medium,
                "f1d",
                &journal,
                &[
                    member_step(
                        a3_member_key("f1d", 0),
                        vec![plain_object(b"a3-f1d-payload-m0")],
                    ),
                    member_step(
                        a3_member_key("f1d", 1),
                        vec![plain_object(b"a3-f1d-payload-m1")],
                    ),
                ],
                10_000,
                false,
                None,
            );
            a3_complete(&mut medium, &facts.mk);
            let limits = AccountingLimits {
                run_entry_buffer: 4,
                max_runs: 64,
                max_scratch_bytes: 1 << 20,
                max_mandatory_identities: 5,
                max_deferred_identities: 2,
                max_shared_references: 1,
                max_namespace_collisions: 1,
                max_protectors_per_deferred: 2,
            };
            let result = account(&mut medium, &facts.mk, 0, &limits, &scratch);
            let (_plan_record, plan) = crate::reclaim::find_installed_plan(&mut medium, &facts.mk)
                .unwrap()
                .unwrap();
            let (expected_mandatory, expected_deferred, expected_shared) =
                derive_expected(&facts, &plan, 0);
            assert_eq!(result.mandatory, expected_mandatory);
            assert_eq!(result.deferred, expected_deferred);
            assert_eq!(result.shared_references, expected_shared);
            assert!(result.scratch.runs_written >= 2, "the streaming is real");
            drop(medium);
            cleanup(&[root, journal, scratch]);
        }
    }

    /// (A3-F3) The pathological limits refuse through the public
    /// entry BEFORE any allocation or scratch file exists, and a
    /// mid-computation failure after the workspace exists cleans
    /// it on the failing path.
    #[cfg(unix)]
    #[test]
    fn the_pathological_limits_and_cleanup_refuse() {
        let (root, mut medium) = a3_store("f3");
        let journal = a3_journal("f3");
        let scratch = a3_scratch("f3");
        let facts = a3_campaign(
            &mut medium,
            "f3",
            &journal,
            &[member_step(
                a3_member_key("f3", 0),
                vec![plain_object(b"a3-f3-payload-m0")],
            )],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        // (a) The pathological limit refuses before the scratch
        // root exists at all.
        let limits = AccountingLimits {
            run_entry_buffer: MAX_ACCOUNTING_RUN_BUFFER_ENTRIES + 1,
            ..AccountingLimits::default()
        };
        let error = match account_batch_retirement(&mut medium, &facts.mk, 0, &limits, &scratch) {
            Ok(_) => panic!("the pathological limit must refuse"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "accounting/limits"
                }
            ),
            "the pathological limit refuses typed: {error:?}"
        );
        assert!(!scratch.exists(), "no allocation or scratch file existed");
        drop(medium);
        cleanup(&[root, journal, scratch]);
        // (b) A mid-computation failure AFTER the workspace, the
        // spool, and the candidate stream exist: a retained
        // anchor's NON-FIRST content object removed behind the
        // open medium's back — every pre-sorter layer reads only
        // first objects (the begin authority's founding descent
        // and the audit-locator discovery), and the walk emits the
        // anchor with its authenticated reference set; the
        // spool-iteration closure's hash-verified read of the
        // second reference refuses, and the failing path cleans
        // the workspace.
        let (root, mut medium) = a3_store("f3b");
        let journal = a3_journal("f3b");
        let scratch = a3_scratch("f3b");
        let first = plain_object(b"a3-f3b-anchor-first");
        let second = plain_object(b"a3-f3b-anchor-second");
        let removed = if first.hash() < second.hash() {
            second.hash()
        } else {
            first.hash()
        };
        let facts = a3_campaign(
            &mut medium,
            "f3b",
            &journal,
            &[
                member_step(
                    a3_member_key("f3b", 0),
                    vec![plain_object(b"a3-f3b-payload-m0")],
                ),
                anchor_step(vec![first, second]),
                member_step(
                    a3_member_key("f3b", 1),
                    vec![plain_object(b"a3-f3b-payload-m1")],
                ),
            ],
            10_000,
            false,
            None,
        );
        a3_complete(&mut medium, &facts.mk);
        std::fs::remove_file(medium_object_path(&root, &removed)).unwrap();
        let error = match account_batch_retirement(
            &mut medium,
            &facts.mk,
            0,
            &AccountingLimits::default(),
            &scratch,
        ) {
            Ok(_) => panic!("the missing closure evidence must refuse"),
            Err(error) => error,
        };
        assert!(
            matches!(error, MediumError::MissingObject { .. }),
            "the spool-iteration closure read refuses typed: {error:?}"
        );
        let residual: Vec<_> = std::fs::read_dir(&scratch).unwrap().collect();
        assert!(
            residual.is_empty(),
            "the failing path cleaned the workspace"
        );
        drop(medium);
        cleanup(&[root, journal, scratch]);
    }
}
