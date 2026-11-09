//! Stage 5 Batch 4: the six-phase transactional crossing — the
//! accepted v1.8 §5.5.1/§5.5.2 protocol binding the real Stage 4
//! simulation (NpcWorld, RegionDirector, the Spine) to the Batch 1–3
//! persistence machinery. This module carries the protocol's
//! preparatory half: the [`DeactivationPlan`] object — built from
//! READ-ONLY sources only (the `Spine::collect_matching` preparation
//! query plus the shared checkpoint gathering), carried whole inside
//! the journal transaction's plan blob (KB-class), and installed
//! id-exactly at phase 5 via `Spine::suspend_ids`. The pipeline-free
//! Stage 4 path never constructs plans, so its behavior and every
//! frozen fingerprint are mechanically untouched.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use engine_core::wire::{MAX_COLLECTION_COUNT, MAX_TAG_LEN};
use engine_core::{
    CompactionError, Decoder, Diagnostics, Encoder, Event, EventClass, EventId, Field, HistoryKind,
    Level, LodLevel, ObligationView, SaveError, Schedule, Spine, Tick,
};

use engine_persist::{
    AppendOutcome, AuthorityMode, AwardCheckpoint, AwardState, CHECKPOINT_METADATA_DOMAIN,
    CONSEQUENCE_BATCH_DOMAIN, CanonicalMedium, CaptureReceipt, CheckpointMetadata, ClassifiedSpan,
    CommitOutcome, CommitPipeline, ConfirmedEvidence, ConsequenceEnvelope, ConsequenceId,
    CounterValues, Coverage, DurabilityClass, FrontierEntry, Hash, JournalRotation, JournalScan,
    KeyPhase, MediumError, OccurrenceSpan, PipelineError, PointerDurability,
    RECLAMATION_COMPLETE_DOMAIN, RECLAMATION_LEDGER_DOMAIN, ReclaimUnit, ReclaimedIndexEntry,
    ReclamationComplete, ReclamationLedger, ReconcileOutcome, Reconciliation, RecoveryAnchor,
    RecoveryError, RevisionRecord, SignalLane, SkipEntry, SourceBaseline, SourcePosition,
    StoredObject, T1Journal, TailState, active_journal_path, chain_range_digest,
    compute_digest_seed, publish_checkpoint_mk, publish_reclamation_ledger,
    read_journal_generations, reconcile, retire_journal_generations, rotate_session_journal,
};

use crate::npc::{NPC_COUNT, NpcWorld, POSITION_DISCARDED, REGION_COUNT, REGION_GRID};
use crate::region::{
    CaptureStatus, LaneSummaryError, RegionCheckpoint, RegionDirector, RegionEntry,
    gather_region_entries, lod_byte, read_flag, read_lod_level,
};
use crate::store::{MediumSource, StoreLocation};

/// One collected obligation — the owned form of a collected
/// [`ObligationView`] (the plan is journalable, so it carries owned
/// data and never borrows the spine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanObligation {
    pub id: EventId,
    pub due: Tick,
    /// The obligation's FULL schedule form — carried verbatim (Batch
    /// 5's lifecycle corrective: a recovered chain resumes with its
    /// original anchoring semantics — logical-time or
    /// execution-relative — never a flattened due; the execution
    /// ledger's schedule-kind record and the follow-up anchoring stay
    /// identical to the never-crashed path).
    pub schedule: Schedule,
    pub class: EventClass,
    pub payload: u64,
    pub field: Field,
    pub depends_on: Vec<EventId>,
}

/// The prepared deactivation transition — the v1.8 §5.5.1 plan object:
/// everything phase 5 needs to install the transition id-exactly,
/// gathered non-destructively (phases 1–2 cost nothing — a pre-durable
/// abort leaves the pre-transition state intact and a retry re-derives
/// identical keys, the episode frontier advancing only on durable
/// commit) and carried whole in the journal transaction's plan blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeactivationPlan {
    /// The transitioning region.
    pub region: usize,
    /// The level before dormancy (its cadence governed the suspended
    /// chain's delays and the reconstruction span estimate).
    pub from_level: LodLevel,
    /// The deactivation's episode — the identity-derivation input
    /// (`ConsequenceId::new(region, episode, npc)`).
    pub episode: u64,
    /// The deactivation tick (the dormant span's label start).
    pub tick: Tick,
    /// The collected obligations in the spine's canonical `(due, seq)`
    /// order — exactly what [`Spine::collect_matching`] with the
    /// region's class predicate returned at preparation; phase 5
    /// suspends exactly these ids.
    pub obligations: Vec<PlanObligation>,
    /// The checkpoint entries (ascending `EventId` order — the legacy
    /// checkpoint's form; the shared gathering produces identical
    /// entries on both paths).
    pub entries: Vec<RegionEntry>,
    /// The captured consequences: `(npc, delta)` — the crossings
    /// moving pending → captured at this boundary, in the entries'
    /// order.
    pub captures: Vec<(usize, u64)>,
}

impl DeactivationPlan {
    /// The plan blob's versioned domain. Schema v2 (Batch 5's
    /// lifecycle corrective): each obligation carries its FULL
    /// schedule form — v1's flattened due reconstructed every
    /// obligation as `AtLogicalTime`, so a recovered chain resumed
    /// with wrong anchoring semantics (the execution ledger's
    /// schedule-kind record and the follow-up anchoring diverged from
    /// the never-crashed path). v1 appeared only within B4-review
    /// test campaigns — no durable artifact carries one — so the walk
    /// reads v2 only; the version bump is the explicit migration
    /// boundary.
    pub const DOMAIN: &'static str = "engine-slice/deactivation-plan/2";

    /// Builds the plan from read-only sources (phases 1–2): the
    /// collected obligations — the caller passes
    /// [`Spine::collect_matching`]'s output for the region's class
    /// predicate — plus the world's current state and the director's
    /// next episode. NO mutation: no suspension, no checkpoint write,
    /// no ledger capture, no scratch clear, no LOD change.
    pub fn prepare(
        region: usize,
        from_level: LodLevel,
        episode: u64,
        tick: Tick,
        world: &NpcWorld,
        collected: &[ObligationView<'_>],
    ) -> Self {
        let obligations = collected
            .iter()
            .map(|view| PlanObligation {
                id: view.id,
                due: view.due,
                schedule: view.schedule,
                class: view.class,
                payload: view.payload,
                field: view.field,
                depends_on: view.depends_on.to_vec(),
            })
            .collect();
        let (entries, captures) = gather_region_entries(world, region, collected);
        DeactivationPlan {
            region,
            from_level,
            episode,
            tick,
            obligations,
            entries,
            captures,
        }
    }

    /// The ids phase 5 suspends — the plan's obligations, id-exactly
    /// (the `Spine::suspend_ids` input).
    pub fn ids(&self) -> Vec<EventId> {
        self.obligations
            .iter()
            .map(|obligation| obligation.id)
            .collect()
    }

    /// The journal plan blob's canonical bytes (domain-tagged,
    /// fixed-width, canonically ordered — the B1 discipline).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SaveError> {
        let mut enc = Encoder::new();
        enc.write_str(Self::DOMAIN);
        enc.write_u64(self.region as u64);
        enc.write_u8(lod_byte(self.from_level));
        enc.write_u64(self.episode);
        enc.write_tick(self.tick);
        enc.write_count(self.obligations.len());
        for obligation in &self.obligations {
            enc.write_u64(obligation.id.seq);
            enc.write_tick(obligation.due);
            match obligation.schedule {
                Schedule::AtLogicalTime { due } => {
                    enc.write_u8(0);
                    enc.write_tick(due);
                }
                Schedule::ExecutionRelative { origin, delay } => {
                    enc.write_u8(1);
                    enc.write_tick(origin);
                    enc.write_u64(delay);
                }
            }
            enc.write_u32(u32::from(obligation.class.0));
            enc.write_u64(obligation.payload);
            enc.write_u8(match obligation.field {
                Field::Near => 0,
                Field::Far => 1,
            });
            enc.write_count(obligation.depends_on.len());
            for dep in &obligation.depends_on {
                enc.write_u64(dep.seq);
            }
        }
        enc.write_count(self.entries.len());
        for entry in &self.entries {
            enc.write_u64(entry.npc as u64);
            enc.write_u64(entry.event.seq);
            enc.write_tick(entry.represented_through);
            enc.write_u64(entry.cursor);
            enc.write_u8(entry.position.0);
            enc.write_u8(entry.position.1);
            enc.write_u8(u8::from(entry.has_signal));
        }
        enc.write_count(self.captures.len());
        for (npc, delta) in &self.captures {
            enc.write_u64(*npc as u64);
            enc.write_u64(*delta);
        }
        Ok(enc.into_bytes())
    }

    /// Restores a plan from its canonical bytes (fail-closed: the
    /// domain gate, bounds, discriminants, canonical orders, and no
    /// trailing — the blob is consumed exactly).
    pub fn restore(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "deactivation-plan";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(Self::DOMAIN)?;
        let region = dec.read_u64()?;
        if region >= REGION_COUNT as u64 {
            return Err(SaveError::Integrity("deactivation-plan/region-bound"));
        }
        let from_level = read_lod_level(&mut dec, SECTION)?;
        let episode = dec.read_u64()?;
        let tick = dec.read_tick()?;
        let obligation_count = dec.read_count(MAX_COLLECTION_COUNT, SECTION, "obligation-count")?;
        let mut obligations = Vec::with_capacity(obligation_count as usize);
        let mut prev: Option<(Tick, u64)> = None;
        for _ in 0..obligation_count {
            let seq = dec.read_u64()?;
            let due = dec.read_tick()?;
            if let Some(prior) = prev
                && (due, seq) <= prior
            {
                return Err(SaveError::Order { section: SECTION });
            }
            prev = Some((due, seq));
            let schedule = match dec.read_u8()? {
                0 => Schedule::AtLogicalTime {
                    due: dec.read_tick()?,
                },
                1 => Schedule::ExecutionRelative {
                    origin: dec.read_tick()?,
                    delay: dec.read_u64()?,
                },
                discriminant => {
                    return Err(SaveError::Enum {
                        section: SECTION,
                        discriminant: u64::from(discriminant),
                    });
                }
            };
            if schedule.due() != due {
                return Err(SaveError::Integrity("deactivation-plan/schedule-due"));
            }
            let class_raw = dec.read_u32()?;
            let Ok(class_value) = u16::try_from(class_raw) else {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(class_raw),
                });
            };
            let payload = dec.read_u64()?;
            let field = match dec.read_u8()? {
                0 => Field::Near,
                1 => Field::Far,
                discriminant => {
                    return Err(SaveError::Enum {
                        section: SECTION,
                        discriminant: u64::from(discriminant),
                    });
                }
            };
            let dep_count = dec.read_count(MAX_COLLECTION_COUNT, SECTION, "dependency-count")?;
            let mut depends_on = Vec::with_capacity(dep_count as usize);
            for _ in 0..dep_count {
                depends_on.push(EventId {
                    seq: dec.read_u64()?,
                });
            }
            obligations.push(PlanObligation {
                id: EventId { seq },
                due,
                schedule,
                class: EventClass(class_value),
                payload,
                field,
                depends_on,
            });
        }
        let entry_count = dec.read_count(NPC_COUNT as u64, SECTION, "entry-count")?;
        let mut entries = Vec::with_capacity(entry_count as usize);
        let mut prev_seq: Option<u64> = None;
        for _ in 0..entry_count {
            let npc = dec.read_u64()?;
            if npc >= NPC_COUNT as u64 {
                return Err(SaveError::Integrity("deactivation-plan/npc-bound"));
            }
            let seq = dec.read_u64()?;
            if let Some(prior) = prev_seq
                && seq <= prior
            {
                return Err(SaveError::Order { section: SECTION });
            }
            prev_seq = Some(seq);
            let represented_through = dec.read_tick()?;
            let cursor = dec.read_u64()?;
            let x = dec.read_u8()?;
            let y = dec.read_u8()?;
            if (x, y) != POSITION_DISCARDED && (x >= REGION_GRID || y >= REGION_GRID) {
                return Err(SaveError::Integrity("deactivation-plan/position-bounds"));
            }
            let has_signal = read_flag(&mut dec, SECTION)?;
            entries.push(RegionEntry {
                npc: npc as usize,
                event: EventId { seq },
                represented_through,
                cursor,
                position: (x, y),
                has_signal,
            });
        }
        let capture_count = dec.read_count(NPC_COUNT as u64, SECTION, "capture-count")?;
        let mut captures = Vec::with_capacity(capture_count as usize);
        let mut seen = HashSet::new();
        for _ in 0..capture_count {
            let npc = dec.read_u64()?;
            if npc >= NPC_COUNT as u64 {
                return Err(SaveError::Integrity("deactivation-plan/npc-bound"));
            }
            if !seen.insert(npc) {
                return Err(SaveError::Integrity("deactivation-plan/capture-duplicate"));
            }
            let delta = dec.read_u64()?;
            captures.push((npc as usize, delta));
        }
        dec.finish()?;
        Ok(DeactivationPlan {
            region: region as usize,
            from_level,
            episode,
            tick,
            obligations,
            entries,
            captures,
        })
    }
}

/// The typed transition failure surface (§5.5.1 case 7): pre-durable
/// failures are retryable soft aborts (nothing was mutated); a
/// phase-5 defensive-validation failure is a detected
/// plan-vs-live incompatibility — a bug, not a runtime condition —
/// whose correct response is the fail-closed session halt (stop
/// authoritative progression, preserve state, report the evidence),
/// never "continue with the region unchanged."
#[derive(Debug)]
pub enum TransitionError {
    /// Phases 3–4 (persist/verify): the journal write or its
    /// verification failed — PRE-durable: the pre-transition state is
    /// intact (nothing was mutated), the partial journal records are
    /// inert by construction, and a later boundary may retry with
    /// identical keys (the episode frontier advances only on durable
    /// commit).
    JournalFailed(PipelineError),
    /// Phase 5 (install): the staged-commit discipline's defensive
    /// validation failed — the prepared plan does not match the live
    /// state (an obligation moved, the episode advanced, a ledger key
    /// already present, a scratch mismatch, or a short suspension
    /// count). This is the fail-closed halt signal.
    InstallIncompatible { region: usize, detail: &'static str },
    /// The chain holds no session snapshot — nothing to recover from
    /// (a founding-only store, or the snapshots were never
    /// published).
    NoSnapshot,
    /// The NEWEST chain-reachable session snapshot is recognizable
    /// but invalid — a session-domain object that fails its restore
    /// gates. This is corruption of the newest authoritative session
    /// state: fail closed, never a silent fallback to an older
    /// snapshot (which would resurrect stale authoritative state).
    CorruptSnapshot(&'static str),
    /// The B3 reconcile rejected the recovered state (a Contract B
    /// incompatibility or a coherence failure) — fail closed with the
    /// evidence.
    Recovery(RecoveryError),
    /// The spine-history compaction refused the requested window
    /// (PP-B2 — nothing to summarize, or the retained window exceeds
    /// the ledger length).
    Compaction(CompactionError),
    /// The coherent activation refusal surfaced BY THE ORCHESTRATED
    /// TICK (PP-B4 checkpoint 3's C3-F4): the director's staged
    /// application refused coherently — the region remains safely
    /// dormant (the checkpoint retained, the held obligations intact,
    /// nothing journaled, nothing half-promoted) and the tick refused
    /// BEFORE the clock advanced. A RETRYABLE soft abort — the
    /// `JournalFailed` class, never the `InstallIncompatible` halt:
    /// the transitions already completed this tick stand (each
    /// individually durable), and the next boundary retries (the
    /// level check skips what completed). Constructed only by
    /// [`crate::orchestration::OrchestratedSession::tick`]; the
    /// crossing's own `activate` preserves the legacy
    /// Ok-on-coherent-refusal semantics for the pipeline-free
    /// callers.
    ActivationRefused { region: usize },
    /// The session-phase refusal (PP-B4 checkpoint 4): a maintenance
    /// compound is active, or the session is Closing — new
    /// authoritative ticks, scheduling, and optional admissions
    /// refuse until the phase returns Active. Nothing ran; the
    /// durable state is untouched. The detail names the phase
    /// ("maintenance-active" or "closing"). Constructed only by the
    /// orchestrated session.
    PhaseRefused { detail: &'static str },
    /// The recovery's allocation-evidence refusal (the S6-C4 stage
    /// 1-A frontier guard): an actionable post-snapshot deactivation
    /// plan references an event identity at or beyond the recovered
    /// snapshot's allocation frontier — the lost session allocated
    /// and executed that identity range after the snapshot, and
    /// that history was never durably saved, so no coherent
    /// identity-preserving reconstruction exists. Refused BEFORE
    /// any inferred mutation or install; the durable campaign state
    /// is untouched; the covering-publication protocol (the C
    /// design) is the durable remedy. Distinguishable from
    /// [`Self::InstallIncompatible`] (a live-state mismatch the
    /// replay COULD have handled) and from
    /// [`Self::RecoveryInvalid`] (a reconstructed state the strict
    /// save/restore contract refuses).
    RecoveryInsufficient {
        region: usize,
        identity: u64,
        frontier: u64,
    },
    /// The recovery's final reconstructed-state validation refusal:
    /// the replayed state fails the engine's own strict
    /// session save/restore roundtrip — the same integrity contract
    /// that would refuse the snapshot after publication. Refused
    /// BEFORE any authoritative progression or canonical
    /// publication; never normalized, repaired, or published. The
    /// detail carries the decoder's own diagnostic.
    RecoveryInvalid { detail: String },
    /// The C1 covering prerequisite's refusal (the recovery-coverage
    /// checkpoint): a nonempty consequential deactivation's T1
    /// capture was refused because its covering snapshot could not
    /// be established — the publication failed, went unknown (and
    /// the reconciliation could not verify the installation), went
    /// stale past the single retry, or the serialized state failed
    /// the strict roundtrip validation. Refused BEFORE the T1
    /// capture and the destructive install: nothing journaled, the
    /// region state unchanged, the crossing coherently retryable.
    /// The uncertain append candidate's revision identity (when one
    /// exists) is carried for the existing reconciliation contract —
    /// never a guessed acceptance, never an unverified append
    /// treated as covered.
    CoveringRefused {
        detail: String,
        candidate: Option<Hash>,
    },
}

/// The explicit session compaction's report (PP-B2 — the GO's §2/§3
/// composition: the spine-history cut plus the consequence-lane
/// summarization, one explicit operation, never a silent default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCompaction {
    /// The spine's history representation after the compaction (the
    /// subsequent snapshot publication takes the `spine-history/2`
    /// form through the spine's own save dispatch).
    pub history: HistoryKind,
    /// The lane summary's per-region boundary actually established
    /// (the highest fully-applied-and-confirmed contiguous episode
    /// per region; monotonic across successive compactions).
    pub lane_boundary: [u64; REGION_COUNT],
    /// The number of consequence identities summarized out of the
    /// retained applied lane.
    pub summarized_identities: u64,
    /// The total summarized applied amount (now carried in the
    /// per-NPC conservation aggregates).
    pub summarized_amount: u64,
}

/// The SK/MK checkpoint publication's report (PP-B3, D-PP-4's
/// two-revision composition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointReport {
    /// The snapshot-publication revision SK (the compacted session
    /// snapshot, published through the existing path).
    pub sk: Hash,
    /// The checkpoint-metadata revision MK (SK's immediate child,
    /// referencing the permanent metadata object).
    pub mk: Hash,
    /// The reclaimable range this checkpoint indexes — the
    /// non-confirming session-snapshot publications between the
    /// boundary (the prior MK or the founding) and SK, in descent
    /// order (newest first).
    pub range: Vec<Hash>,
    /// The skip-table size (the self-retirement entry included).
    pub skips: u64,
    /// The range digest commitment (the previously-verified
    /// commitment over the range's record bytes).
    pub range_digest: [u8; 32],
}

/// The two-phase reclamation's report (PP-B3, D-PP-4's
/// verify-before-reclaim protocol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclamationReport {
    /// The audit revision on the chain (the reclamation-ledger/1
    /// object's identity — installed before any delete, the
    /// restart's recognition point).
    pub ledger: Hash,
    /// The checkpoint whose range was reclaimed (its metadata object
    /// is permanent — the exact identities, keys, epochs, and skips).
    pub mk: Hash,
    /// The retired snapshot (reclaimed together with its range — the
    /// whole-revision unit).
    pub sk: Hash,
    /// The eligible record count (the checkpoint's index length).
    pub eligible_records: u64,
    /// The objects removed BY THIS CALL (idempotent — a restart
    /// reports only what remained).
    pub removed_objects: u64,
    /// The journal files removed (empty when no rotation preceded —
    /// the legacy journal IS the active one then, never touched).
    pub journal_files: Vec<PathBuf>,
}

impl From<PipelineError> for TransitionError {
    fn from(error: PipelineError) -> Self {
        TransitionError::JournalFailed(error)
    }
}

impl From<MediumError> for TransitionError {
    fn from(error: MediumError) -> Self {
        TransitionError::JournalFailed(PipelineError::Medium(error))
    }
}

/// The deactivation's capture envelope — the award form (§5.4.1): a
/// fresh identity's first consequential record, uniform `Captured`
/// units over the identity's occurrence span `(0, delta]` — the
/// crossings this boundary claims — with the expected prior and
/// baseline at the identity's empty base and the boundary's source
/// position.
fn capture_envelope(
    region: usize,
    episode: u64,
    npc: usize,
    delta: u64,
    now: Tick,
) -> Result<ConsequenceEnvelope, SaveError> {
    ConsequenceEnvelope::capture(
        ConsequenceId::new(region as u32, episode, npc as u32),
        vec![ClassifiedSpan {
            span: OccurrenceSpan::new(0, delta)
                .map_err(|_| SaveError::Integrity("crossing/capture-span"))?,
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
            tick: now.as_u64(),
        },
    )
}

/// The six-phase durable crossing session: the real Stage 4 simulation
/// bound to a campaign's canonical medium and T1 journal through the
/// Batch 3 pipeline. One instance owns the pipeline for the session;
/// the pipeline-free Stage 4 path never constructs one, so its
/// behavior and every frozen fingerprint are mechanically untouched.
pub struct SessionCrossing<S: MediumSource = StoreLocation> {
    pipeline: CommitPipeline<S::Medium>,
    /// The campaign's canonical-medium SOURCE (PP-B4 checkpoint 1's
    /// ownership seam): fresh medium handles for the snapshot
    /// publications, the maintenance walks, and the lifecycle
    /// checkpoint (each handle's coordination is per-operation, so
    /// additional handles serialize safely alongside the pipeline's
    /// own). The legacy instantiation is the store location itself —
    /// fresh direct provider handles, exactly the accepted behavior;
    /// a future orchestrated instantiation supplies the single
    /// I/O worker's proxy source so every in-session canonical
    /// access routes through the one injected medium.
    source: S,
    /// The campaign's journal ROOT — the single authoritative
    /// active-path location (the review corrective F1): every
    /// rotation publishes its pointer at `<root>.active`, and the
    /// recovery and the backup resolve the active journal from that
    /// one root, across any number of successive rotations. Distinct
    /// from the pipeline's ACTIVE journal path after a rotation (the
    /// pipeline holds the rotated generation; the root never moves).
    journal_root: PathBuf,
    /// THE AUTHORITY GUARD (the review correctives R4 + R5): the
    /// crossing's usability for authoritative progression. `Lost`
    /// (R4): a rotation failed after the pointer changed — the
    /// in-memory pipeline holds a journal the root no longer
    /// selects; only a fresh recovery repairs it.
    /// `DurabilityUnresolved` (R5): the pointer flipped but its
    /// power-loss durability is unresolved — the pre-rotation
    /// journal is a superset only of the evidence at rotation time,
    /// never of future writes to the new journal, so NO further
    /// authoritative T1 write may proceed until the durability is
    /// re-established (the repair method) or a verified recovery
    /// repairs it.
    journal_authority: JournalAuthority,
    /// The capture-validity generation (PP-B4 checkpoint 3, the F1
    /// preflight): the journal-pointer flip count. Incremented ONLY by
    /// a successful pointer flip (the rotation mechanism's Ok) —
    /// BEFORE the R5 durability gate, so a flip followed by an
    /// unresolved durability or a failed rebuild still invalidates
    /// every earlier capture (the R5 repair never un-flips the
    /// pointer). Private mutation; the read-only accessor is
    /// [`Self::journal_flip_count`].
    journal_flips: u64,
    /// THE C1 COVERING TRACK (the recovery-coverage checkpoint): the
    /// last covering snapshot THIS crossing established — the exact
    /// content identity of the authoritative state it captured, and
    /// the accepted revision that carries it. The equivalence check
    /// (a previously accepted snapshot satisfies a later plan's
    /// prerequisite ONLY when the current state serializes to the
    /// SAME content identity) consults this track; every
    /// authority-side mutation produces different bytes and requires
    /// a fresh covering. Private to the prerequisite machinery.
    covering: Option<CoveringSnapshot>,
}

/// The C1 covering track's entry: the exact-state binding (the
/// content identity of the captured authoritative state) and the
/// accepted revision that published it (the diagnostic identity —
/// which revision carries the covering, retained for introspection).
struct CoveringSnapshot {
    content_hash: Hash,
    revision: Hash,
}

/// The crossing's journal-authority state (the review correctives
/// R4 + R5).
enum JournalAuthority {
    /// Healthy: authoritative progression permitted.
    Healthy,
    /// R4: the pointer changed but the pipeline rebuild failed —
    /// unrecoverable in-process; only a fresh recovery repairs it.
    Lost,
    /// R5: the pointer flipped but its power-loss durability is
    /// unresolved — repairable in-process via the durability
    /// re-establishment.
    DurabilityUnresolved,
}

/// The legacy directory conveniences, pinned to the store-location
/// instantiation (PP-B4 checkpoint 1): every other constructor and
/// method is source-generic — these two exist so the accepted
/// directory-based call sites compile unchanged.
impl SessionCrossing<StoreLocation> {
    /// Opens (or joins) the campaign store: the file medium over
    /// `store_dir` — founded with a standalone authority record on
    /// first open, so every confirmation carries the honest
    /// `T3-local (standalone)` label — joined with the caller's
    /// journal in one pipeline. The caller constructs the journal (a
    /// plain open, or a fault-injected open for the failure matrix).
    /// The existing directory-based constructor — behaviorally
    /// identical to the accepted implementation.
    pub fn open(
        store_dir: &Path,
        journal: T1Journal,
        campaign: &str,
    ) -> Result<Self, PipelineError> {
        Self::open_at(
            &StoreLocation::Directory(store_dir.to_path_buf()),
            journal,
            campaign,
        )
    }

    /// The crash recovery: reconstruct the session from the durable
    /// state alone — the latest chain snapshot plus the journal — and
    /// re-open the crossing for the continuation (the pending T1
    /// re-drive available through [`Self::commit_canonical`]).
    ///
    /// The composition (§5.4/§5.5.2):
    /// 1. **The snapshot** — the latest chain-reachable session
    ///    snapshot, restored through both codecs' gates. A
    ///    recognizable-but-invalid NEWEST session object fails closed
    ///    (corruption of the newest authoritative state — never a
    ///    silent fallback to an older snapshot, which would resurrect
    ///    stale state).
    /// 2. **The journal scan + the actionability filter** — a durably
    ///    CANCELED transaction is never replayed: its plan never
    ///    installs, its captures never restore.
    /// 3. **The interleaved crossing restore + plan install** — per
    ///    ACTIONABLE transaction in journal-sequence order, only the
    ///    deactivations that POSTDATE the snapshot (the plan's episode
    ///    above the SNAPSHOT's director episode — the snapshot-origin
    ///    discriminator, never a journal-constructed counter): the
    ///    claimed crossings the snapshot had NOT already materialized
    ///    (the claimed delta minus the snapshot's scratch for each
    ///    NPC's first post-snapshot capture, the full delta after —
    ///    the prior install's capture zeroed the scratch exactly as
    ///    the live sequence did) restore into the world, then the plan
    ///    installs through the standard phase-5 bundle; an IMPLICIT
    ///    reactivation between episodes is derived from the next
    ///    plan's own from-level (the durable evidence the region was
    ///    live) and replayed through the real activation path — the
    ///    legacy install plus the live application of the prior
    ///    episode's captures, nothing new journaled. A PRE-snapshot
    ///    deactivation is already materialized in the snapshot's
    ///    ledger and world — never re-applied.
    /// 4. **The coherence verification** — the B3 reconcile with the
    ///    REAL canonical-chain confirmation evidence (the re-opened
    ///    pipeline's chain walk, never an empty map) over the anchor
    ///    built from the RECOVERED SESSION STATE (the ledger's own
    ///    captured amounts; the applied claims admitted only under the
    ///    per-NPC credits conservation): every actionable journal
    ///    envelope suppressed by the session's materialization or the
    ///    canonical evidence — anything else is a claim the verified
    ///    state cannot account for, the halt.
    /// 5. **The re-opened crossing** — a fresh pipeline over the same
    ///    journal (the captured keys seeded from the journal).
    pub fn recover(
        store_dir: &Path,
        journal_path: &Path,
        campaign: &str,
    ) -> Result<(Spine<NpcWorld>, RegionDirector, Self), TransitionError> {
        Self::recover_at(
            &StoreLocation::Directory(store_dir.to_path_buf()),
            journal_path,
            campaign,
        )
    }
}

impl<S: MediumSource> SessionCrossing<S> {
    /// The provider-aware constructor (PP-B1's integration seam,
    /// source-generalized at PP-B4 checkpoint 1): the same
    /// founding-and-joining semantics over the explicit medium
    /// source — the legacy store location (the file root or the
    /// SQLite campaign database) or a future injected source.
    /// **No silent selection ever occurs**: a database at a
    /// fresh path is unfounded until the founding append runs (the
    /// same first-open semantics as the file medium); an existing
    /// campaign at the location is joined, never re-founded.
    /// (F4 §6) THE ENTRY-POINT CLOSURE: an already-BOUND canonical
    /// campaign joins only through its bound journal — the supplied
    /// journal must be the binding's pointer-resolved ACTIVE
    /// generation with the exact bound identities (a foreign,
    /// legacy, stale, or incorrect-root journal refuses before any
    /// authoritative progression or rotation), the crossing's
    /// journal root is the BINDING's authoritative root, and a
    /// supplied campaign name must agree with the existing
    /// founding's persisted identity. A legacy (unbound) campaign
    /// keeps the accepted behavior; the trusted bound
    /// bootstrap/recovery remains the authorized path for bound
    /// campaigns.
    pub fn open_at(source: &S, journal: T1Journal, campaign: &str) -> Result<Self, PipelineError> {
        let mut medium = source.open_medium()?;
        if medium.head()?.is_none() {
            // The founding record is the application's setup act; on a
            // fresh single-writer session the append must accept —
            // anything else surfaces typed for investigation.
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0);
            match medium.append(&founding, &[], None)? {
                AppendOutcome::Accepted { .. } => {}
                _ => return Err(PipelineError::UnfoundedMedium),
            }
        }
        // (F4 §6) THE ENTRY-POINT CLOSURE: the campaign's canonical
        // founding and installed binding are verified BEFORE the
        // pipeline joins the caller's journal — the supplied campaign
        // name must agree with the existing founding's persisted
        // identity, and a bound campaign accepts only its own bound
        // active journal.
        let founding = engine_persist::reclaim::founding_identity_medium(&mut medium)?;
        let founding_record = medium.read_record(&founding)?;
        match &founding_record.kind {
            engine_persist::record::RecordKind::Founding { campaign_id, .. }
                if campaign_id == campaign => {}
            engine_persist::record::RecordKind::Founding { .. } => {
                return Err(PipelineError::Medium(MediumError::Chain {
                    rule: "journal-root/campaign-mismatch",
                }));
            }
            _ => {
                return Err(PipelineError::Medium(MediumError::Chain {
                    rule: "journal-root/founding-not-root",
                }));
            }
        }
        let installed =
            engine_persist::reclaim::find_installed_journal_root_binding(&mut medium, &founding)?;
        let journal_root = match installed {
            Some((_id, binding)) => {
                // THE BOUND JOIN: the supplied journal must be the
                // binding's pointer-resolved ACTIVE generation, its
                // bound identities verified — never a foreign,
                // legacy, or stale journal.
                let root = std::path::PathBuf::from(binding.location.clone());
                let active = active_journal_path(&root)?;
                if journal.path() != active {
                    return Err(PipelineError::Medium(MediumError::Chain {
                        rule: "journal-root/foreign-journal",
                    }));
                }
                engine_persist::journal::scan_bound_file(
                    &active,
                    &binding.founding,
                    &binding.root_id,
                )
                .map_err(PipelineError::Journal)?;
                // The crossing's journal root is the BINDING's
                // authoritative root — the single location the
                // rotation pointer and the recovery resolve from.
                root
            }
            None => journal.path().to_path_buf(),
        };
        let pipeline = CommitPipeline::open(medium, journal)?;
        Ok(Self {
            pipeline,
            source: source.clone(),
            journal_root,
            journal_authority: JournalAuthority::Healthy,
            journal_flips: 0,
            covering: None,
        })
    }

    /// (V2-W F4) THE TRUSTED BOUND BOOTSTRAP — the one operation
    /// that may establish a campaign's journal-root binding. THE
    /// JOURNAL-FIRST RECOVERABLE ORDERING (F4-R8/§7): (0) the
    /// founding identity — deterministic in the campaign name for a
    /// truly fresh campaign, discovered from the chain for an
    /// existing one; (1) the stable root ID, deterministically
    /// derived from the founding and the normalized location; (2)
    /// the durable bound-journal creation (or strict reuse) at the
    /// selected root BEFORE the founding append and any transaction
    /// — under the F4-R11 creation authority (the verified
    /// binding-absence capability; once a binding is installed no
    /// public path can create or repair the journal), adopting an
    /// existing complete residue only when it is EMPTY (F4-R12: the
    /// trusted ordering journals nothing before the binding, so
    /// committed transactions, cancellations, or an unresolved
    /// non-creation tail are foreign history, refused with the
    /// bytes preserved), a torn pre-binding creation resolving
    /// (nothing could have been journaled, and no binding exists yet
    /// to make it damaged evidence, F4-R9), a legacy, foreign, or
    /// conflicting header refusing; (3) the canonical founding
    /// append (the truly fresh campaign only); (4) the canonical
    /// binding publication through the trusted in-crate admission
    /// (the CAS, the exact-duplicate recognition, the
    /// conflicting-root refusal, the Unknown reconciliation — never
    /// a false completion; the first-binding fresh-shape gate, the
    /// root-ID derivation check, the durable journal-bootstrap
    /// evidence verification, and the canonical location descriptor
    /// check, F4-R8/R13); (5) the installed agreement verified; (6)
    /// only then the authoritative pipeline. THE CRASH STATES: a
    /// re-entry with the binding ALREADY installed is the
    /// completed-bootstrap resume — the journal must EXIST and carry
    /// a COMPLETE matching bound header (a missing journal is
    /// MISSING authoritative evidence, a torn or corrupt header is
    /// DAMAGED evidence — both fail closed, never repaired, never
    /// recreated); a re-entry after the journal creation but before
    /// the founding append (the truly fresh medium) reuses the
    /// journal and proceeds; a re-entry after the founding append
    /// but before the binding publication reuses the journal and
    /// republishes (the exact-duplicate convergence). (F4-R5/§7) A
    /// LEGACY CAMPAIGN — an existing unbound chain with history
    /// beyond the founding, a founding identity disagreeing with the
    /// supplied campaign name, or a founding-only chain with NO
    /// pre-existing bound journal at the root (the state
    /// indistinguishable from a legacy campaign whose historical
    /// journal lives elsewhere) — refuses typed; nothing is
    /// migrated, no history discarded (the owner-level migration
    /// contract owns that case).
    pub fn open_bound_bootstrap(
        source: &S,
        journal_path: &Path,
        campaign: &str,
    ) -> Result<Self, TransitionError> {
        let mut medium = source.open_medium().map_err(PipelineError::Medium)?;
        let fresh_campaign = medium.head()?.is_none();
        // (F4-R8/§7) THE JOURNAL-FIRST ORDERING: the founding record's
        // identity is DETERMINISTIC in the campaign name (the
        // standalone founding at epoch 0, no content), so the durable
        // bound journal is created (or reused) BEFORE the founding
        // append. Every crash state of the trusted bootstrap therefore
        // leaves the journal as its distinguishing durable evidence —
        // and a campaign that was NOT truly fresh at entry (an
        // existing chain) may bind only onto that PRE-EXISTING
        // evidence, never a fresh creation: the founding-only chain
        // with no journal at the root could be a legacy campaign (its
        // historical journal elsewhere, its unconfirmed work real) and
        // is refused rather than silently migrated.
        let founding = if fresh_campaign {
            RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0).id()
        } else {
            let founding = engine_persist::reclaim::founding_identity_medium(&mut medium)
                .map_err(PipelineError::Medium)?;
            // (F4-R5) THE CAMPAIGN-IDENTITY AGREEMENT: the supplied
            // campaign name must agree with the canonical founding's
            // persisted campaign identity — a mismatched name never
            // opens another campaign's bound session.
            let founding_record = medium
                .read_record(&founding)
                .map_err(PipelineError::Medium)?;
            match &founding_record.kind {
                engine_persist::record::RecordKind::Founding { campaign_id, .. }
                    if campaign_id == campaign => {}
                engine_persist::record::RecordKind::Founding { .. } => {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "journal-root/campaign-mismatch",
                    });
                }
                _ => {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "journal-root/founding-not-root",
                    });
                }
            }
            founding
        };
        let location =
            engine_persist::reclaim::JournalRootBinding::normalize_location(journal_path);
        let root_id = engine_persist::reclaim::journal_root_id(&founding, &location);
        let installed_binding = if fresh_campaign {
            None
        } else {
            engine_persist::reclaim::find_installed_journal_root_binding(&mut medium, &founding)
                .map_err(PipelineError::Medium)?
        };
        if let Some((_id, installed)) = &installed_binding {
            // THE BINDING-FIRST RE-ENTRY: an already-bound campaign
            // resumes only on the EXACT bound root with the journal
            // present and COMPLETE (a torn or corrupt header is
            // damaged evidence, never repaired here — F4-R9).
            if installed.root_id != root_id || installed.location != location {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "journal-root/binding-conflict",
                });
            }
            let active = engine_persist::active_journal_path(journal_path)
                .map_err(PipelineError::Journal)?;
            engine_persist::journal::scan_bound_file(&active, &founding, &root_id).map_err(
                |error| TransitionError::InstallIncompatible {
                    region: 0,
                    detail: match &error {
                        engine_persist::JournalError::Io { source, .. }
                            if source.kind() == std::io::ErrorKind::NotFound =>
                        {
                            "journal-root/missing-journal"
                        }
                        _ => "journal-root/active-corrupt",
                    },
                },
            )?;
            let journal = T1Journal::open_bound(&active, &founding, &root_id)
                .map_err(PipelineError::Journal)?;
            return Self::finish_bound_open(source, medium, journal, journal_path);
        }
        // (F4-R5) THE LEGACY-UNBOUND REFUSAL: an existing campaign
        // with history beyond the founding — a legacy lineage, its
        // historical journal real — never acquires a bound journal
        // through this path (the no-automatic-migration rule; the
        // historical journal is preserved and untouched). The
        // founding-head shape proceeds to the §7 evidence rule below.
        if !fresh_campaign && medium.head()? != Some(founding) {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "journal-root/legacy-unbound",
            });
        }
        // (§7) THE NON-FRESH REUSE-ONLY RULE: a campaign that was not
        // truly fresh at entry may bind ONLY onto the interrupted
        // bootstrap's durable residue — the bound journal already
        // existing at the root. A fresh creation over an existing
        // founding-only chain could be a legacy campaign being
        // silently re-rooted (its unconfirmed historical journal
        // discarded); the owner-level migration contract owns that
        // case, never this path.
        if !fresh_campaign && !journal_path.exists() {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "journal-root/missing-bootstrap-evidence",
            });
        }
        // (F4-R12) THE RESIDUE CLASSIFICATION — read-only, BEFORE the
        // founding append and the journal-root binding publication:
        // an existing journal at the root is adoptable as
        // interrupted-bootstrap residue ONLY when it matches the
        // expected identities and carries NO authoritative history —
        // the trusted bootstrap's ordering journals nothing before
        // the binding, so committed transactions, cancellations, or
        // an unresolved non-creation tail are FOREIGN history,
        // refused with the bytes preserved (never silently adopted
        // as a fresh campaign's authoritative root, never
        // truncated). A legacy, foreign, or corrupt header refuses
        // the same way; and (Option B) an existing empty file or a
        // torn creation prefix is PRESERVED and refused (the
        // conservative ambiguous-recovery policy — the automatic
        // repair is prohibited).
        if journal_path.exists() {
            let residue_bytes = std::fs::read(journal_path).map_err(|_error| {
                TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "journal-root/residue-unreadable",
                }
            })?;
            match engine_persist::journal::read_bound_header(&residue_bytes, &founding, &root_id) {
                Err(_error) => {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "journal-root/residue-invalid",
                    });
                }
                Ok(engine_persist::journal::BoundHeaderState::Matching { .. }) => {
                    let scan =
                        engine_persist::journal::scan_bound_file(journal_path, &founding, &root_id)
                            .map_err(|_error| TransitionError::InstallIncompatible {
                                region: 0,
                                detail: "journal-root/residue-invalid",
                            })?;
                    if !scan.committed.is_empty()
                        || !scan.cancellations.is_empty()
                        || scan.tail != TailState::Clean
                    {
                        return Err(TransitionError::InstallIncompatible {
                            region: 0,
                            detail: "journal-root/residue-history",
                        });
                    }
                }
                Ok(engine_persist::journal::BoundHeaderState::Fresh) => {
                    // (Option B) An existing zero-byte file is
                    // preserved — never treated as a fresh creation.
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "journal-root/residue-empty",
                    });
                }
                Ok(engine_persist::journal::BoundHeaderState::TornCreation { .. }) => {
                    // (Option B) THE CONSERVATIVE AMBIGUOUS-RECOVERY
                    // POLICY (the owner's selection): the automatic
                    // repair of a torn creation prefix is prohibited —
                    // the evidence is preserved and the bootstrap
                    // refuses (a separately authorized recovery
                    // operation applies; not implemented in this
                    // checkpoint).
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "journal-root/residue-torn",
                    });
                }
            }
        }
        // (F4-R11/R11-A) THE SINGLE CREATION OPERATION: the current
        // canonical lifecycle verification (the authenticated
        // binding absence, at this moment, against THIS campaign's
        // own medium — failures propagate; an installed binding
        // refuses) and the creation/repair happen indivisibly in one
        // call. No detachable authority exists anywhere in the API;
        // the authorized root location and root ID are derived
        // within the operation from this call's founding and journal
        // root; and a missing journal is created only at a root
        // showing no prior journal life (the unrelated-empty-medium
        // aim at an established root refuses).
        let journal = engine_persist::reclaim::create_campaign_bound_journal(
            &mut medium,
            journal_path,
            &founding,
        )
        .map_err(|error| match error {
            engine_persist::reclaim::BoundCreationError::Medium(error) => {
                PipelineError::Medium(error)
            }
            engine_persist::reclaim::BoundCreationError::Journal(error) => {
                PipelineError::Journal(error)
            }
        })?;
        // (1) The canonical founding append — the truly fresh
        // campaign only (an existing chain already carries it).
        if fresh_campaign {
            let founding_record = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0);
            match medium.append(&founding_record, &[], None)? {
                AppendOutcome::Accepted { .. } => {}
                _ => return Err(PipelineError::UnfoundedMedium.into()),
            }
        }
        // (4) The canonical binding publication (the trusted
        // admission's own CAS/duplicate/conflict/Unknown discipline,
        // the fresh-shape gate, the root-ID derivation check, and the
        // durable journal-bootstrap evidence verification (F4-R8);
        // the re-entry after a crash-between converges through the
        // exact-duplicate recognition).
        let tick = medium
            .head()?
            .and_then(|head| medium.read_record(&head).ok())
            .map(|record| record.tick.saturating_add(1))
            .unwrap_or(1);
        let (_binding_id, outcome) = engine_persist::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding,
            &root_id,
            &location,
            tick,
        )
        .map_err(PipelineError::Medium)?;
        // (5) The installed agreement verified (the admission's
        // outcome is the acknowledgment; the discovery re-reads the
        // canonical installation).
        let (verified_id, verified) =
            engine_persist::reclaim::find_installed_journal_root_binding(&mut medium, &founding)
                .map_err(PipelineError::Medium)?
                .ok_or(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "journal-root/bootstrap-unresolved",
                })?;
        if verified.root_id != root_id
            || verified.location != location
            || !matches!(
                outcome,
                engine_persist::reclaim::JournalRootBindingOutcome::Accepted { id } if id == verified_id
            ) && !matches!(
                outcome,
                engine_persist::reclaim::JournalRootBindingOutcome::DuplicateVerified { id } if id == verified_id
            ) && !matches!(
                outcome,
                engine_persist::reclaim::JournalRootBindingOutcome::ReconciledInstalled { id } if id == verified_id
            )
        {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "journal-root/bootstrap-unresolved",
            });
        }
        // (6) Only now the authoritative pipeline.
        Self::finish_bound_open(source, medium, journal, journal_path)
    }

    /// (F4-R2/R3/R7) THE TRUSTED CROSSING-SCOPED V2 PLAN PUBLICATION —
    /// the production route. Requires the healthy journal and
    /// worker authority; verifies the exact installed canonical
    /// campaign/root binding (the capability issuance); and holds
    /// the correct serialization boundary across the obligation
    /// collection and the canonical plan append, PROVEN by the
    /// nonforgeable lifetime-bound publication permit: the
    /// crossing's OWN active-journal handle already holds the
    /// root's exclusive sidecar lock when the active IS the root
    /// (the permit borrows that held lock — no reacquisition, no
    /// same-process deadlock), and an explicitly acquired root lock
    /// (the permit issued from it) when the active is a rotated
    /// generation (the rotation holds the same lock across its
    /// flip — the publication cannot race the rotation interval).
    /// All the F1-F5, W2, and W3A gates, the duplicate-content
    /// verification, and the installed-plan immutability are inside
    /// the authorized core. NON-DESTRUCTIVE.
    pub fn publish_reclamation_plan_bound(
        &mut self,
        mk: &engine_persist::Hash,
        ledger_byte_limit: u64,
        tick: u64,
    ) -> Result<
        (
            engine_persist::Hash,
            engine_persist::reclaim::PlanPublicationOutcome,
        ),
        TransitionError,
    > {
        self.assert_journal_authority()?;
        let mut medium = self.source.open_medium().map_err(PipelineError::Medium)?;
        let authority = engine_persist::reclaim::verify_bound_journal_authority(
            &mut medium,
            &self.journal_root,
        )
        .map_err(PipelineError::Medium)?;
        let active = active_journal_path(&self.journal_root).map_err(PipelineError::Journal)?;
        if active == self.journal_root {
            // THE UNROTATED NO-REACQUISITION PATH: the crossing's own
            // active-journal handle IS the root's lock holder — the
            // permit borrows it (the core verifies the path
            // agreement; a same-process second acquire would
            // deadlock, and none happens).
            let permit = self.pipeline.journal().publication_permit();
            engine_persist::reclaim::publish_reclamation_plan_authorized(
                &mut medium,
                mk,
                ledger_byte_limit,
                &authority,
                &permit,
                &self.journal_root,
                tick,
            )
        } else {
            // THE ROTATED PATH: the crossing holds only the active
            // generation's lock — the root's exclusive sidecar lock
            // is acquired explicitly across the collection and the
            // canonical append (the same boundary a concurrent
            // rotation honors).
            let guard = engine_persist::journal::hold_publication_lock(&self.journal_root)
                .map_err(PipelineError::Journal)?;
            engine_persist::reclaim::publish_reclamation_plan_authorized(
                &mut medium,
                mk,
                ledger_byte_limit,
                &authority,
                &guard.permit(),
                &self.journal_root,
                tick,
            )
        }
        .map_err(|error| TransitionError::JournalFailed(PipelineError::Medium(error)))
    }

    /// The bound opening's shared tail: the pipeline over the bound
    /// journal handle, the crossing bound to the campaign root.
    fn finish_bound_open(
        source: &S,
        medium: S::Medium,
        journal: T1Journal,
        journal_path: &Path,
    ) -> Result<Self, TransitionError> {
        let journal_root = journal_path.to_path_buf();
        let pipeline = CommitPipeline::open(medium, journal)?;
        Ok(Self {
            pipeline,
            source: source.clone(),
            journal_root,
            journal_authority: JournalAuthority::Healthy,
            journal_flips: 0,
            covering: None,
        })
    }

    /// THE AUTHORITY GUARD (the review correctives R4 + R5, composed
    /// with PP-B4 checkpoint 2's worker gate): a crossing whose
    /// rotation failed after the authoritative pointer changed — or
    /// whose pointer flip's power-loss durability remains unresolved
    /// — is explicitly unusable for further authoritative
    /// progression: no T1 write may silently continue through a
    /// journal the campaign root no longer selects, and none may
    /// precede a durably established pointer (the pre-rotation
    /// journal is a superset only of the evidence at rotation time,
    /// never of future writes to the new journal). Every mutation
    /// refuses until the durability is re-established (the R5
    /// repair) or a fresh recovery re-derives the crossing from the
    /// pointer. The medium-level `checkpoint` boundary stays usable
    /// (it never writes through the journal). And a dead or poisoned
    /// canonical-medium source (the worker instantiation's health
    /// cell, PP-B4 checkpoint 2) refuses everything closed — never a
    /// silent continuation against an uncertain canonical state.
    fn assert_journal_authority(&self) -> Result<(), TransitionError> {
        match self.journal_authority {
            JournalAuthority::Healthy => {}
            JournalAuthority::Lost => {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "rotation/authority-lost",
                });
            }
            JournalAuthority::DurabilityUnresolved => {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "rotation/pointer-durability",
                });
            }
        }
        // THE WORKER POISON GATE (PP-B4 checkpoint 2): a dead or
        // poisoned canonical-medium worker fails every authoritative
        // operation closed — journal-only deactivations included. The
        // legacy store-location instantiation is always alive (the
        // default), so its behavior is unchanged.
        if !self.source.alive() {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "worker/unavailable",
            });
        }
        Ok(())
    }

    /// The journal-pointer flip count — the capture-validity
    /// generation (PP-B4 checkpoint 3): read-only, the mutation
    /// authority is the rotation's own (private). A snapshot capture
    /// recording this value is admissible only while the crossing
    /// still reports the same value.
    pub fn journal_flip_count(&self) -> u64 {
        self.journal_flips
    }

    /// The C1 covering track's accepted revision — the diagnostic
    /// identity of the revision carrying the current covering (None
    /// before any covering is established). Read-only, the mutation
    /// authority is the covering prerequisite's own (private); the
    /// test and introspection surface.
    #[doc(hidden)]
    pub fn covering_revision(&self) -> Option<Hash> {
        self.covering.as_ref().map(|covering| covering.revision)
    }

    /// Sets the campaign journal ROOT (crate-trusted — the
    /// orchestrated session's recovery path re-derives the crossing
    /// over the pointer-resolved ACTIVE journal, then restores the
    /// root so future rotations publish the pointer at the one
    /// authoritative location).
    pub(crate) fn set_journal_root(&mut self, root: PathBuf) {
        self.journal_root = root;
    }

    /// Seeds the C1 covering track from an ACCEPTED session-snapshot
    /// publication of the exact state `content_hash` (crate-trusted —
    /// the orchestrated session's publication completions and the
    /// snapshot publications' own accepted appends call this): the
    /// durable snapshot covers any nonempty plan prepared from that
    /// same state, and the equivalence check consults the track
    /// instead of colliding on the chain-unique content-hash
    /// idempotency key. **MONOTONIC:** a candidate qualifies only
    /// when it is chain-NEWER than the current entry — the accepted
    /// records' EPOCHS, the trustworthy chain-sequencing evidence,
    /// never a content-hash comparison — so a delayed older
    /// completion never regresses a newer accepted covering, while a
    /// genuinely newer accepted snapshot updates the entry. The first
    /// entry seeds unconditionally; unreadable ordering evidence
    /// conservatively retains the current entry (never a guessed
    /// promotion); the same record is idempotent.
    pub(crate) fn seed_covering(&mut self, content_hash: Hash, revision: Hash) {
        if let Some(current) = &self.covering {
            if current.revision == revision {
                return;
            }
            let Ok(mut medium) = self.source.open_medium() else {
                return;
            };
            let (Ok(current_epoch), Ok(candidate_epoch)) = (
                medium
                    .read_record(&current.revision)
                    .map(|record| record.epoch),
                medium.read_record(&revision).map(|record| record.epoch),
            ) else {
                return;
            };
            if candidate_epoch < current_epoch {
                return;
            }
        }
        self.covering = Some(CoveringSnapshot {
            content_hash,
            revision,
        });
    }

    /// The C1 covering track's current binding, if any (crate-trusted
    /// — the orchestrated recovery transfers the restored snapshot's
    /// covering identity onto its fresh worker-backed crossing, the
    /// same track the plain recovery's own crossing carries).
    pub(crate) fn covering_snapshot(&self) -> Option<(Hash, Hash)> {
        self.covering
            .as_ref()
            .map(|covering| (covering.content_hash, covering.revision))
    }

    /// The composed authority gate, exposed for the orchestrated
    /// session (PP-B4 checkpoint 3): the R4/R5 journal-authority
    /// states AND the worker-health admission — exactly the check
    /// every authoritative crossing operation performs.
    pub fn assert_authoritative(&self) -> Result<(), TransitionError> {
        self.assert_journal_authority()
    }

    /// The graceful lifecycle boundary (PP-B1): the ratified `TRUNCATE`
    /// checkpoint for the SQLite provider — a no-op for the file
    /// medium (its every publication already fsyncs through the
    /// nine-step contract). Typed failures, never swallowed.
    pub fn checkpoint(&mut self) -> Result<(), MediumError> {
        self.source.open_medium()?.checkpoint()
    }

    /// Reconciles the retained pending canonical attempt (PP-B4
    /// checkpoint 4's maintenance gate): an uncertain REQUIRED
    /// confirmation — a possibly-committed consequence batch whose
    /// reply was lost — resolves against the canonical chain BEFORE
    /// any maintenance compound. Installed confirms through the
    /// verified receipt (the slot clears only after it);
    /// NotInstalled leaves the retained attempt for the coherent
    /// retry; Unverifiable retains the uncertainty — never a guessed
    /// confirmation. The pipeline's own exception-safe lifecycle,
    /// exposed for the orchestrated session's maintenance path.
    pub fn reconcile_pending(&mut self) -> Result<ReconcileOutcome, TransitionError> {
        self.assert_journal_authority()?;
        self.pipeline
            .reconcile_pending()
            .map_err(TransitionError::JournalFailed)
    }

    /// Retries the retained pending canonical attempt (PP-B4
    /// checkpoint 4's close settlement, C4-F3): after a verified
    /// NotInstalled — or a stale parent — the accepted
    /// exception-safe retry path re-walks the chain (determining
    /// keys another writer may have confirmed — never re-submitted),
    /// then re-submits the remaining per-key identities at the
    /// verified head — no duplicate consequential identity. The
    /// pipeline's own lifecycle, exposed for the orchestrated
    /// session's graceful close.
    pub fn retry_pending(&mut self) -> Result<CommitOutcome, TransitionError> {
        self.assert_journal_authority()?;
        self.pipeline
            .retry_pending()
            .map_err(TransitionError::JournalFailed)
    }

    /// The six-phase transactional deactivation (§5.5.1/§5.5.2), over
    /// the real simulation state:
    ///
    /// 1. **Validate** — the region is live (a Dormant target from a
    ///    live level; the caller's presence decision).
    /// 2. **Determine** — the [`DeactivationPlan`] from read-only
    ///    sources only.
    /// 3. **Persist T1** — ONE journal transaction when crossings
    ///    were captured: the capture envelopes (one per captured NPC
    ///    — the award form, uniform `Captured` units) plus the
    ///    complete plan as the plan blob. An empty-scratch
    ///    deactivation journals nothing (the return is `None`) — the
    ///    LOD transition is purely re-derivable from the presence, so
    ///    only the crossings need durable-before-discard protection.
    /// 4. **Verify** — the receipt exists only after the journal's
    ///    fsync (the B3 contract); an ambiguous write was reconciled
    ///    by reread+verify inside the capture.
    /// 5. **Install** — the staged prepare/install bundle
    ///    ([`install_deactivation`]) at the same tick boundary.
    ///
    /// A pre-durable failure (phases 3–4) is a retryable soft abort —
    /// the pre-transition state stands. A phase-5 defensive failure is
    /// the fail-closed halt signal. The return is the journaled
    /// transaction's receipt, or `None` when nothing was captured.
    pub fn deactivate(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        director: &mut RegionDirector,
        region: usize,
        now: Tick,
    ) -> Result<Option<CaptureReceipt>, TransitionError> {
        self.transition(spine, director, region, now, |_, _| {})
    }

    /// The durable reactivation: the captured consequences commit to
    /// credits as the LIVE APPLICATION of the already-durable
    /// captures — the B3 per-key lattice's `JournaledT1 →
    /// AppliedToLivePending` promotion, reported through
    /// `mark_applied` — followed by the legacy activation's install
    /// (exactly-once application, labeled reconstruction, resume with
    /// original identity, reclassify, promote).
    ///
    /// Nothing NEW is journaled: the captures are already durable
    /// (the deactivation's transaction), and the captured→credits
    /// transfer is re-derivable from the ledger plus this activation
    /// — the B3 pipeline's per-key model is ONE identity, ONE
    /// envelope, ONE lifecycle, so a second (movement) envelope under
    /// the capture's identity would be a conflicting reuse; the
    /// recovery instead re-drives the pending captures and a later
    /// activation re-applies them. A coherent activation refusal is
    /// the LEGACY retry semantics — nothing new was journaled, the
    /// captures stay pending and actionable in the ledger, the region
    /// safely dormant — never a durable-intent abandonment.
    pub fn activate(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        director: &mut RegionDirector,
        region: usize,
        to: LodLevel,
        now: Tick,
    ) -> Result<(), TransitionError> {
        self.assert_journal_authority()?;
        // Phase 1 — validate: dormant with a checkpoint, and the
        // ledger statuses coherent (the legacy phase-0 standard — an
        // unknown key the checkpoint claims is incoherent recovery
        // state: the halt, never a silent promotion).
        if director.levels()[region] != LodLevel::Dormant {
            return Err(TransitionError::InstallIncompatible {
                region,
                detail: "activate-of-live",
            });
        }
        let state = &director.states()[region];
        let Some(checkpoint) = &state.checkpoint else {
            return Err(TransitionError::InstallIncompatible {
                region,
                detail: "checkpoint-absent",
            });
        };
        let episode = state.episode;
        let mut pending: Vec<ConsequenceId> = Vec::new();
        for entry in checkpoint.entries.iter().filter(|entry| entry.has_signal) {
            let id = ConsequenceId::new(region as u32, episode, entry.npc as u32);
            match director.ledger().status(id) {
                CaptureStatus::Pending(_) => pending.push(id),
                CaptureStatus::Applied => {}
                CaptureStatus::Unknown => {
                    return Err(TransitionError::InstallIncompatible {
                        region,
                        detail: "ledger-key-unknown",
                    });
                }
            }
        }
        // Phase 5 — install: the legacy activation (validation,
        // exactly-once application, reconstruction, resume, promote).
        // A refusal is the legacy coherent retry — the captures stay
        // pending, the region dormant.
        if director.transition_region(spine, region, to, now) && !pending.is_empty() {
            // Report the live application (the per-key promotion —
            // the pending-vs-confirmed distinction, never conflated
            // with durability; journaled keys only, after the
            // application actually happened).
            self.pipeline.mark_applied(&pending)?;
        }
        Ok(())
    }

    /// The per-key pipeline phase for one identity (the durable
    /// status surface — the gating demonstration's input).
    pub fn key_phase(&self, id: ConsequenceId) -> Option<KeyPhase> {
        self.pipeline.status(id).map(|status| status.phase)
    }

    /// Test-only (the failure matrix's seam): the six phases with the
    /// caller's gap inserted between durability (phase 4) and the
    /// install (phase 5) — the injection point for the
    /// defensive-failure regressions. Engine code never calls this.
    #[doc(hidden)]
    pub fn debug_deactivate_around_install(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        director: &mut RegionDirector,
        region: usize,
        now: Tick,
        gap: impl FnOnce(&mut Spine<NpcWorld>, &mut RegionDirector),
    ) -> Result<Option<CaptureReceipt>, TransitionError> {
        self.transition(spine, director, region, now, gap)
    }

    /// The shared six-phase driver (the gap is the test seam — a
    /// no-op in production).
    fn transition(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        director: &mut RegionDirector,
        region: usize,
        now: Tick,
        gap: impl FnOnce(&mut Spine<NpcWorld>, &mut RegionDirector),
    ) -> Result<Option<CaptureReceipt>, TransitionError> {
        self.assert_journal_authority()?;
        // Phase 1 — validate.
        let from = director.levels()[region];
        if from == LodLevel::Dormant {
            return Err(TransitionError::InstallIncompatible {
                region,
                detail: "deactivate-of-dormant",
            });
        }
        // Phase 2 — determine (read-only).
        let class = EventClass::new(region as u16);
        let episode = director.states()[region].episode + 1;
        let collected = spine.collect_matching(|view| view.class == class);
        let plan = DeactivationPlan::prepare(region, from, episode, now, spine.world(), &collected);
        // Phase 2.5 — THE C1 COVERING PREREQUISITE (the
        // recovery-coverage checkpoint): a NONEMPTY consequential
        // plan may not journal its T1 transaction until an ACCEPTED,
        // validated session snapshot of the EXACT authoritative state
        // that plan was prepared from is durable on the chain (the
        // empty-scratch deactivation journals nothing non-re-derivable
        // and needs no covering). The plan's preparation (phase 2)
        // and the snapshot's serialization read the SAME state with
        // no authority-side mutation between — the phases are
        // single-threaded and the prerequisite is I/O-only — so the
        // binding is exact, never a frontier or tick heuristic. See
        // [`Self::establish_covering`] for the full contract.
        if !plan.captures.is_empty() {
            self.establish_covering(spine, director)?;
        }
        // The C1 crash-window seam (test-only): the hard exit AFTER
        // the covering's acceptance and BEFORE the T1 capture.
        after_covering_seam();
        // Phases 3–4 — persist + verify: ONE journal transaction —
        // the capture envelopes plus the plan blob — SKIPPED when
        // nothing was captured: an empty-scratch deactivation has no
        // non-re-derivable content to protect (the LOD transition is
        // a pure function of the presence, so a post-crash boundary
        // re-derives it against the snapshot); only the crossings
        // are real-execution history the journal must make durable.
        // The envelopes stage in ConsequenceId-sorted order (one
        // transaction covers one region and episode, so npc order IS
        // the identity order — the journal's canonical discipline:
        // capture records land in input order and the commit's
        // identity list is written sorted, validated equal at every
        // scan).
        let receipt = if plan.captures.is_empty() {
            None
        } else {
            let plan_bytes = plan.canonical_bytes().map_err(PipelineError::Format)?;
            let mut captures = plan.captures.clone();
            captures.sort_by_key(|(npc, _)| *npc);
            for (npc, delta) in &captures {
                let envelope = capture_envelope(region, episode, *npc, *delta, now)
                    .map_err(PipelineError::Format)?;
                self.pipeline.stage(envelope)?;
            }
            Some(self.pipeline.capture(
                SourcePosition {
                    head: None,
                    tick: now.as_u64(),
                },
                DurabilityClass::DurableEligible,
                Vec::new(),
                vec![FrontierEntry {
                    region: region as u32,
                    episode,
                }],
                plan_bytes,
            )?)
        };
        // Phase 4 — verify: the receipt is post-fsync by contract.
        gap(spine, director);
        // Phase 5 — install (the staged bundle).
        install_deactivation(spine, director, &plan)?;
        Ok(receipt)
    }

    /// THE C1 COVERING PREREQUISITE (the recovery-coverage
    /// checkpoint's conservative rule): before a nonempty
    /// consequential deactivation plan journals its T1 transaction,
    /// an ACCEPTED, validated session snapshot of the EXACT
    /// authoritative state that plan was prepared from must be
    /// durable on the chain. The procedure:
    ///
    /// 1. **Serialize** the current authoritative state (the same
    ///    `save_session` machinery every publication uses).
    /// 2. **The exact-state equivalence check**: a previously
    ///    accepted covering snapshot satisfies this prerequisite only
    ///    when the current state serializes to the SAME content
    ///    identity — byte-exact, never a frontier or tick heuristic
    ///    (any mutation since — an install, a compaction, a
    ///    reclassification — produces different bytes and requires a
    ///    fresh covering).
    /// 3. **The strict roundtrip validation**: the serialized bytes
    ///    must pass `restore_session` — the same integrity contract
    ///    the recovery's final validation enforces — BEFORE
    ///    publication.
    /// 4. **Publish** through the existing canonical source (the
    ///    single I/O worker in the orchestrated instantiation; a
    ///    fresh provider handle per operation in the legacy form) —
    ///    the same CAS append every snapshot publication uses, with
    ///    the same content-hash idempotency key.
    /// 5. **Settle**: the `Accepted` outcome (the append executed,
    ///    the CAS won, the chain rules passed, the provider's actual
    ///    durability tier carried) establishes the covering; a
    ///    stale parent retries ONCE against the observed head (the
    ///    same validated bytes — the equivalence binding is the
    ///    CONTENT, not the parent); an `Unknown` outcome reconciles
    ///    under the lock — `Installed` establishes the covering,
    ///    anything else REFUSES the crossing BEFORE the T1 capture
    ///    and the destructive install, the uncertain candidate's
    ///    identity preserved in the typed refusal for the existing
    ///    reconciliation contract (the recovery's
    ///    `reconcile_unknown`, the maintenance harvest). No
    ///    unverified append is ever treated as accepted.
    ///
    /// **The state-freeze guarantee:** between the plan's
    /// preparation (phase 2) and the T1 capture (phase 3) the
    /// crossing performs only this I/O-only prerequisite — no
    /// simulation tick, no scheduling, no rotation, no maintenance
    /// can interleave (the authority thread is inside the crossing;
    /// the journal-authority guard already refuses the closed
    /// states) — so the prepared plan's preconditions remain
    /// exactly the snapshot's state. Complete event identities,
    /// allocation statistics, execution history, dependencies,
    /// region state, and consequence-ledger evidence are all inside
    /// the captured state; nothing is renumbered, fabricated, or
    /// relaxed.
    fn establish_covering(
        &mut self,
        spine: &Spine<NpcWorld>,
        director: &RegionDirector,
    ) -> Result<(), TransitionError> {
        let bytes = save_session(spine, director).map_err(PipelineError::Format)?;
        let object = StoredObject::new(bytes)?;
        let content_hash = object.hash();
        if let Some(covering) = &self.covering
            && covering.content_hash == content_hash
        {
            // The exact-state equivalence: the previously accepted
            // covering snapshot captured THIS state — the plan is
            // covered (the refused-crossing retry and the
            // shared-state cases).
            return Ok(());
        }
        // The strict roundtrip validation, BEFORE publication.
        restore_session(object.bytes()).map_err(|error| TransitionError::CoveringRefused {
            detail: format!("covering/roundtrip-validation: {error:?}"),
            candidate: None,
        })?;
        let mut medium = self.source.open_medium()?;
        let parent = medium.head()?.ok_or(PipelineError::UnfoundedMedium)?;
        let mut attempt =
            covering_append(&mut medium, &object, content_hash, parent, spine.clock())?;
        let mut candidate = attempt.candidate;
        let mut outcome = match attempt.outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                return self.resolve_covering_append_error(
                    &mut medium,
                    error,
                    candidate,
                    content_hash,
                );
            }
        };
        if let AppendOutcome::RejectedStaleParent {
            observed: Some(observed),
        } = outcome
        {
            // ONE retry against the observed head — the same
            // validated bytes (the equivalence binding is the
            // content, not the parent). The retried attempt's own
            // candidate identity replaces the first's: the uncertain
            // window's reconciliation must name exactly the record
            // the retry built (the observed parent, not the stale
            // one).
            attempt = covering_append(&mut medium, &object, content_hash, observed, spine.clock())?;
            candidate = attempt.candidate;
            outcome = match attempt.outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    return self.resolve_covering_append_error(
                        &mut medium,
                        error,
                        candidate,
                        content_hash,
                    );
                }
            };
        }
        let revision = match outcome {
            AppendOutcome::Accepted { id, .. } => id,
            AppendOutcome::RejectedStaleParent { .. } => {
                return Err(TransitionError::CoveringRefused {
                    detail: "covering/stale-parent".into(),
                    candidate: None,
                });
            }
            AppendOutcome::Unknown { .. } => {
                // The uncertain append: reconcile under the lock —
                // Installed establishes the covering (the candidate
                // accepted); anything else refuses the crossing with
                // the candidate's identity preserved for the existing
                // reconciliation contract. The candidate is exactly
                // the identity the append built — the same bytes, the
                // same parent — so an installed covering is never
                // misclassified as absent. A reconciliation that
                // ITSELF fails (the transport, the lock) is never
                // converted into a success or a raw escape: the
                // crossing refuses with the candidate preserved.
                let reconciliation = medium.reconcile_unknown(&candidate).map_err(|error| {
                    TransitionError::CoveringRefused {
                        detail: format!("covering/reconcile-unavailable: {error:?}"),
                        candidate: Some(candidate),
                    }
                })?;
                return match reconciliation {
                    Reconciliation::Installed { .. } => {
                        self.covering = Some(CoveringSnapshot {
                            content_hash,
                            revision: candidate,
                        });
                        Ok(())
                    }
                    Reconciliation::NotInstalled { .. } => Err(TransitionError::CoveringRefused {
                        detail: "covering/unknown-not-installed".into(),
                        candidate: Some(candidate),
                    }),
                    Reconciliation::Unverifiable { reason } => {
                        Err(TransitionError::CoveringRefused {
                            detail: format!("covering/unknown-unverifiable: {reason}"),
                            candidate: Some(candidate),
                        })
                    }
                };
            }
        };
        self.covering = Some(CoveringSnapshot {
            content_hash,
            revision,
        });
        Ok(())
    }

    /// Resolves a covering-append ERROR (never an outcome): the
    /// duplicate-idempotency-key rejection means a record keyed by
    /// THIS content may already be durable — an accepted publication
    /// of the exact state whose completion was never consumed (the
    /// unpolled optional submission). The rejection alone is never
    /// acceptance: the record is resolved against the chain — its
    /// idempotency key, its single content reference, and the strict
    /// session roundtrip over the referenced bytes — and only a
    /// fully verified record satisfies the prerequisite (the track
    /// binds to it; no second append of the same content). Any other
    /// append error propagates typed, exactly as before.
    fn resolve_covering_append_error(
        &mut self,
        medium: &mut dyn CanonicalMedium,
        error: MediumError,
        candidate: Hash,
        content_hash: Hash,
    ) -> Result<(), TransitionError> {
        if !matches!(
            &error,
            MediumError::Chain {
                rule: "duplicate-idempotency-key"
            }
        ) {
            return Err(error.into());
        }
        let refusal = |detail: String| TransitionError::CoveringRefused {
            detail,
            candidate: Some(candidate),
        };
        let mut cursor = medium
            .head()
            .map_err(|error| refusal(format!("covering/duplicate-resolution: {error:?}")))?;
        while let Some(id) = cursor {
            let record = medium
                .read_record(&id)
                .map_err(|error| refusal(format!("covering/duplicate-resolution: {error:?}")))?;
            // The EXACT admissible shape: the required covering
            // publication is a canonical SINGLE-snapshot-object record
            // — precisely one content reference, and it IS the
            // expected snapshot. A key-matched record carrying extra
            // references (a legitimate multi-object publication) is a
            // different record shape and never covering evidence,
            // however valid its objects.
            let matches = record.idempotency_key.as_deref() == Some(content_hash.as_bytes())
                && record.content_refs.len() == 1
                && record.content_refs.first() == Some(&content_hash);
            if matches {
                // The strict session roundtrip over the referenced
                // bytes: an invalid or unreadable snapshot never
                // qualifies as a covering through identity alone.
                let bytes = medium.read_object(&content_hash).map_err(|error| {
                    refusal(format!("covering/duplicate-content-unverified: {error:?}"))
                })?;
                restore_session(&bytes).map_err(|error| {
                    refusal(format!("covering/duplicate-invalid-session: {error:?}"))
                })?;
                self.covering = Some(CoveringSnapshot {
                    content_hash,
                    revision: id,
                });
                return Ok(());
            }
            cursor = record.parent;
        }
        Err(refusal("covering/duplicate-unresolved".into()))
    }

    /// Publishes the session snapshot as ONE verified content object
    /// on the campaign chain (the B2 nine-step append — atomic,
    /// chain-reachable, content-verified): a regular revision whose
    /// single content reference is the snapshot object and whose
    /// idempotency key is the snapshot's content hash. Returns the
    /// revision's identity.
    pub fn publish_snapshot(
        &mut self,
        spine: &Spine<NpcWorld>,
        director: &RegionDirector,
    ) -> Result<Hash, TransitionError> {
        self.assert_journal_authority()?;
        let bytes = save_session(spine, director).map_err(PipelineError::Format)?;
        let object = StoredObject::new(bytes)?;
        let snapshot_hash = object.hash();
        // The C1 validation provenance: this publication seeds the
        // covering track — an accepted snapshot of the exact state
        // covers any nonempty plan prepared from it — so the bytes
        // must pass the SAME strict session roundtrip the covering
        // prerequisite demands, BEFORE publication (an invalid
        // serialization never becomes covering evidence).
        restore_session(object.bytes()).map_err(|error| TransitionError::CoveringRefused {
            detail: format!("covering/publication-validation: {error:?}"),
            candidate: None,
        })?;
        // A fresh medium handle (each provider's coordination is
        // per-operation; the pipeline's own handle is idle between its
        // operations).
        let mut medium = self.source.open_medium()?;
        let head = medium.head()?.ok_or(PipelineError::UnfoundedMedium)?;
        let parent_epoch = medium.read_record(&head)?.epoch;
        let record = RevisionRecord::regular(head, parent_epoch, spine.clock().as_u64())
            .with_idempotency_key(snapshot_hash.as_bytes().to_vec())
            .with_content_refs(vec![snapshot_hash]);
        match medium.append(&record, std::slice::from_ref(&object), Some(head))? {
            AppendOutcome::Accepted { id, .. } => {
                // The publication seeds the C1 covering track: an
                // accepted snapshot of THIS exact state covers any
                // nonempty plan prepared from it (the equivalence
                // check consults the track) — through the MONOTONIC
                // seed (the accepted record is the chain's new head,
                // so the ordering check passes by construction).
                self.seed_covering(snapshot_hash, id);
                Ok(id)
            }
            _ => Err(PipelineError::UnfoundedMedium.into()),
        }
    }

    /// The explicit, versioned session compaction (PP-B2 — the GO's
    /// §2/§3 composition): the consequence-lane summarization under
    /// the ratified eligibility rule, plus the spine-history cut, as
    /// ONE explicit operation — never a silent default, never
    /// automatic on the existing paths (the pipeline-free and
    /// crossing lifecycles publish full fidelity unless the operator
    /// compacts). The subsequent [`Self::publish_snapshot`] takes the
    /// `spine-history/2` form through the spine's own save dispatch.
    ///
    /// **The lane eligibility** (per region, the contiguous ascending
    /// episode prefix): an episode is summarizable iff NONE of its
    /// identities is pending (pending captures are referenced — the
    /// dormant region's outstanding held obligations and the T1
    /// intent) and EVERY applied identity of it is canonically
    /// confirmed (the chain's per-key evidence — `is_confirmed`).
    /// Identities fully materialized and confirmed are unreferenced
    /// by definition; the retained lanes keep everything an
    /// outstanding reference can reach. The boundary is monotonic
    /// across successive compactions.
    ///
    /// **The spine cut**: `retain_tail` ledger records stay verbatim;
    /// the prefix is summarized into the authenticated SHA-256 chain
    /// (see [`Spine::compact`]).
    ///
    /// **The failure atomicity** (the review corrective): a REJECTED
    /// compound compaction leaves both session components unchanged.
    /// The spine-window preflight (phase 0) decides the compound's own
    /// typed refusals read-only, BEFORE the lane summarization can
    /// mutate anything; the lane summarization itself is
    /// all-or-nothing (its coherence refusals precede every mutation,
    /// and its arithmetic refusals fire on local state only); and
    /// after the preflight passes, the spine cut cannot refuse — so
    /// no error path observes a partially compacted session.
    pub fn compact_session(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        director: &mut RegionDirector,
        retain_tail: u64,
    ) -> Result<SessionCompaction, TransitionError> {
        self.assert_journal_authority()?;
        // Phase 0 — the spine-window preflight: the compound's typed
        // refusals for a degenerate window are decided BEFORE any
        // session component mutates (the lane summarization must
        // never run only for a rejected spine cut to strand a
        // half-compacted session). The check is read-only (the
        // ledger length), its conditions are exactly [`Spine::compact`]'s
        // own, and after it passes the phase-3 cut cannot refuse —
        // nothing between here and there touches the spine.
        let ledger_len = spine.ledger_retained().len() as u64;
        if retain_tail > ledger_len {
            return Err(TransitionError::Compaction(
                CompactionError::RetainExceedsLedger {
                    ledger_len,
                    retain: retain_tail,
                },
            ));
        }
        if ledger_len - retain_tail == 0 {
            return Err(TransitionError::Compaction(
                CompactionError::NothingToSummarize {
                    ledger_len,
                    retain: retain_tail,
                },
            ));
        }
        // Phase 1 — the lane eligibility walk.
        let mut boundary = director
            .ledger()
            .summarized()
            .map(|summary| summary.boundary)
            .unwrap_or([0; REGION_COUNT]);
        for (region, slot) in boundary.iter_mut().enumerate() {
            let mut pending_episodes: BTreeSet<u64> = BTreeSet::new();
            let mut applied_by_episode: BTreeMap<u64, Vec<ConsequenceId>> = BTreeMap::new();
            for (id, _) in director.ledger().pending_entries() {
                if id.region as usize == region {
                    pending_episodes.insert(id.episode);
                }
            }
            for (id, _) in director.ledger().applied_entries() {
                if id.region as usize == region && id.episode > *slot {
                    applied_by_episode.entry(id.episode).or_default().push(id);
                }
            }
            for (episode, ids) in applied_by_episode {
                if pending_episodes.contains(&episode) {
                    break;
                }
                if ids.iter().all(|id| self.pipeline.is_confirmed(*id)) {
                    *slot = episode;
                } else {
                    break;
                }
            }
        }
        // Phase 2 — the lane summarization (the prunes into the
        // conservation aggregates; the typed refusals are the
        // state-incompatibility class — the eligibility verification
        // above makes them unreachable in coherent states, and a
        // refusal here is the fail-closed signal, never a silent
        // strand).
        let (summarized_identities, summarized_amount) = director
            .summarize_lanes(boundary)
            .map_err(|error| match error {
                LaneSummaryError::BoundaryRegression { region, .. } => {
                    TransitionError::InstallIncompatible {
                        region,
                        detail: "lane-boundary-regression",
                    }
                }
                LaneSummaryError::RetainedBelowBoundary { id } => {
                    TransitionError::InstallIncompatible {
                        region: id.region as usize,
                        detail: "lane-retained-below-boundary",
                    }
                }
                LaneSummaryError::TotalsOverflow => TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "lane-totals-overflow",
                },
            })?;
        // Phase 3 — the spine history cut (the explicit versioned
        // operation; typed refusals for a degenerate window).
        let history = spine
            .compact(retain_tail)
            .map_err(TransitionError::Compaction)?;
        Ok(SessionCompaction {
            history,
            lane_boundary: boundary,
            summarized_identities,
            summarized_amount,
        })
    }

    /// The SK/MK checkpoint publication (PP-B3 — D-PP-4's ratified
    /// two-revision composition): ONE explicit operation composing
    /// the existing disciplines —
    ///
    /// 1. **The session compaction** ([`Self::compact_session`] —
    ///    the F1 preflight inside; the spine takes the
    ///    `spine-history/2` form, the lanes summarize under the
    ///    eligibility rule).
    /// 2. **SK** — the compacted session snapshot publication (the
    ///    existing [`Self::publish_snapshot`] path; SK is a normal
    ///    snapshot, recoverable through the established recovery —
    ///    and if the compound below fails, an ordinary valid
    ///    publication, never a fabricated checkpoint).
    /// 3. **The range walk** — from SK's parent down to the boundary
    ///    (the prior checkpoint's MK, or the founding), classifying
    ///    each record by its content object's domain: the
    ///    session-snapshot publications (this checkpoint's OWN
    ///    reclaimable range), the confirming revisions (retained
    ///    anchors — never reclaimable, D-PP-4a), the checkpoint
    ///    metadata (the prior MK — the permanent boundary). The
    ///    skip table (every retained record standing on a reclaimed
    ///    parent jumps to its nearest retained ancestor, plus the
    ///    non-circular self-retirement entry), the compact
    ///    reclaimed-record index ({key?, hash, epoch} — both
    ///    historical identity classes, D-PP-4b's rejection honored),
    ///    and the digest commitment (the running SHA-256 over the
    ///    range's record bytes, oldest first, seeded from the
    ///    founding — computed while the records are still present:
    ///    the previously-verified commitment).
    /// 4. **MK** — SK's IMMEDIATE child, one content object (the
    ///    permanent metadata), the deterministic checkpoint key. An
    ///    `Unknown` outcome reconciles under the existing lock-held
    ///    discipline; a retry is idempotent; anything unresolved
    ///    fails closed.
    ///
    /// The crash-after-SK-before-MK window (the mandatory PP-B3
    /// regression): SK stands as an ordinary valid snapshot — the
    /// recovery finds it, no checkpoint status is fabricated, no
    /// reclaim eligibility exists (the reclamation requires the
    /// completed MK) — and a retry re-derives and completes MK
    /// safely.
    ///
    /// An empty range refuses (the typed `checkpoint-empty-range`):
    /// an explicit checkpoint that indexes nothing is not a
    /// checkpoint — every published MK is meaningful.
    pub fn publish_checkpoint(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        director: &mut RegionDirector,
        retain_tail: u64,
    ) -> Result<CheckpointReport, TransitionError> {
        self.assert_journal_authority()?;
        // Phase 1 — the session compaction (the preflight discipline
        // inside compact_session).
        let _compaction = self.compact_session(spine, director, retain_tail)?;
        // Phase 2 — SK: the compacted snapshot publication (the
        // existing path).
        let sk = self.publish_snapshot(spine, director)?;
        // The genuine SK-committed/MK-uncommitted crash seam
        // (test-only — see [`ARM_AFTER_SK_EXIT`]): a death here
        // leaves the ordinary snapshot standing with no checkpoint
        // status — the mandatory regression's exact boundary.
        after_sk_seam();
        // Phase 3 — the range walk + the metadata computation (a
        // fresh medium handle; the crossing's established pattern).
        let mut medium = self.source.open_medium()?;
        let sk_record = medium.read_record(&sk).map_err(PipelineError::Medium)?;
        let sk_epoch = sk_record.epoch;
        let Some(sk_parent) = sk_record.parent else {
            return Err(PipelineError::UnfoundedMedium.into());
        };
        let (walked, _boundary, seed) = walk_checkpoint_range(&mut medium, sk_parent)?;
        let mut range = Vec::new();
        let mut index = Vec::new();
        let mut chain_bytes = Vec::new();
        for entry in &walked {
            if matches!(entry.kind, RangeKind::Publication) {
                range.push(entry.record.id());
                index.push(ReclaimedIndexEntry::new(
                    entry.record.idempotency_key.clone(),
                    entry.record.id(),
                    entry.record.epoch,
                ));
                chain_bytes.push(entry.record.canonical_bytes());
            }
        }
        if range.is_empty() {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "checkpoint-empty-range",
            });
        }
        // SK's own retirement entry (the mandatory condition 2): the
        // self-retirement's identity and key join the index — the
        // membership universe preserves both after the whole-revision
        // reclamation (a retired SK's key can never be reused).
        index.push(ReclaimedIndexEntry::new(
            sk_record.idempotency_key.clone(),
            sk,
            sk_epoch,
        ));
        let skips = compute_checkpoint_skips(sk_parent, &walked);
        // The digest: the range's canonical bytes OLDEST first (the
        // reverse of the descent order).
        chain_bytes.reverse();
        let range_digest = chain_range_digest(&seed, &chain_bytes);
        let metadata = CheckpointMetadata::new(sk, sk_parent, skips, index, range_digest, seed);
        // Phase 4 — MK: the immediate child. An Unknown reconciles
        // against the candidate; a NotInstalled reconciles into one
        // idempotent retry (the deterministic key); anything still
        // unresolved fails closed — never a guessed outcome.
        let tick = spine.clock().as_u64();
        let (mk_candidate, outcome) =
            publish_checkpoint_mk(&mut medium, sk, sk_epoch, &metadata, tick)?;
        let mk = match outcome {
            AppendOutcome::Accepted { id, .. } => id,
            AppendOutcome::RejectedStaleParent { .. } => {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "checkpoint-mk-parent-not-sk",
                });
            }
            AppendOutcome::Unknown { .. } => {
                match medium
                    .reconcile_unknown(&mk_candidate)
                    .map_err(PipelineError::Medium)?
                {
                    Reconciliation::Installed { id } => id,
                    Reconciliation::NotInstalled { .. } => {
                        let (_, retry) =
                            publish_checkpoint_mk(&mut medium, sk, sk_epoch, &metadata, tick)?;
                        match retry {
                            AppendOutcome::Accepted { id, .. } => id,
                            _ => {
                                return Err(TransitionError::InstallIncompatible {
                                    region: 0,
                                    detail: "checkpoint-mk-unresolved",
                                });
                            }
                        }
                    }
                    Reconciliation::Unverifiable { .. } => {
                        return Err(TransitionError::InstallIncompatible {
                            region: 0,
                            detail: "checkpoint-mk-unverifiable",
                        });
                    }
                }
            }
        };
        Ok(CheckpointReport {
            sk,
            mk,
            range,
            skips: metadata.skips.len() as u64,
            range_digest,
        })
    }

    /// The explicit T1 journal rotation (PP-B3, D-PP-4's §4.4
    /// survival rules, the conservative form) over the live
    /// single-writer session: each committed transaction is
    /// classified against the CHAIN-VERIFIED anchor — the newest
    /// session snapshot's lane-summary boundary (never the caller's
    /// unpublished in-memory state) — and the chain-derived
    /// confirmed set. A transaction retires only when it is canceled
    /// (inert by construction) or when EVERY one of its consequential
    /// identities is canonically confirmed AND subsumed below the
    /// verified anchor's lane boundary (materialized-and-confirmed-
    /// subsumed, the §4.4 retirement rule); everything else survives
    /// — the outstanding unconfirmed work, the plans, and the
    /// confirmed-but-unmaterialized captures (their episodes sit
    /// above the boundary until the reactivation materializes them).
    /// The surviving set is written to a fresh re-anchored journal
    /// (the mechanism's own scan verification and atomic pointer
    /// flip), and this crossing's pipeline reopens over the new
    /// journal (the keys reseeded from it, the confirmed set
    /// re-derived from the chain; the staged in-memory accrual is
    /// honestly lost, P0-5). The legacy journal is retained on disk,
    /// untouched, until the reclamation boundary. The synchronous
    /// single-writer discipline closes the swap/flip window — no
    /// append can interleave.
    pub fn rotate_journal(&mut self) -> Result<JournalRotation, TransitionError> {
        self.assert_journal_authority()?;
        // (THE AUTHENTICATED RETENTION) The rotation's subsumption
        // boundary is derived IN-CRATE by the session-bound entry —
        // from the newest canonical session snapshot (the same
        // HEAD-descent selection rule as the recovery), hash-verified,
        // version-checked, and strictly decoded through the shared
        // session-anchor module. No boundary is computed or supplied
        // here; the crossing and the entry share ONE retirement
        // algorithm.
        // (F4-R11-B) THE SESSION-BOUND ROTATION ENTRY: the bound
        // identities are PROBED in-crate through the pipeline's own
        // medium (never caller-supplied — a bound root can never
        // take the legacy arm, a legacy root never the bound arm),
        // and the retention classification is DERIVED from the
        // pipeline's own journal (the live active-journal handle —
        // the lock ownership covers the transition) and confirmed
        // set (§4.4: retire iff canceled, or actionable with every
        // identity confirmed AND subsumed below the verified
        // anchor's boundary — the boundary is the trusted session's
        // chain-verified claim). No caller-invented retention
        // evidence exists anywhere in the API; the raw wrappers are
        // crate-private.
        let rotation =
            rotate_session_journal(&mut self.pipeline, &self.journal_root).map_err(|error| {
                match error {
                    engine_persist::SessionRotationError::Medium(error)
                    | engine_persist::SessionRotationError::Anchor(
                        engine_persist::session_anchor::AnchorError::Medium(error),
                    ) => PipelineError::Medium(error).into(),
                    engine_persist::SessionRotationError::Journal(error) => {
                        PipelineError::Journal(error).into()
                    }
                    engine_persist::SessionRotationError::Anchor(
                        engine_persist::session_anchor::AnchorError::NoAnchor,
                    ) => TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "journal-rotation/no-anchor",
                    },
                    engine_persist::SessionRotationError::Anchor(
                        engine_persist::session_anchor::AnchorError::UnsupportedSnapshotVersion,
                    ) => TransitionError::CorruptSnapshot("snapshot-version-unsupported"),
                    engine_persist::SessionRotationError::Anchor(
                        engine_persist::session_anchor::AnchorError::CorruptSnapshot,
                    ) => TransitionError::CorruptSnapshot("anchor-decode-failed"),
                }
            })?;
        // THE CAPTURE-VALIDITY GENERATION (PP-B4 checkpoint 3, the F1
        // preflight): the mechanism returned Ok — the pointer FLIPPED —
        // so every snapshot capture taken before this instant is
        // invalidated, EVEN IF the durability is unresolved or the
        // rebuild fails (the R5 repair never un-flips the pointer; a
        // pre-flip capture must never become admissible again). A
        // pre-flip mechanism failure leaves the counter untouched (the
        // pointer never moved; earlier captures remain valid).
        self.journal_flips += 1;
        // THE DURABILITY GATE (the review corrective R5): the
        // pointer flip succeeded (the live authority IS the new
        // journal), but an UNRESOLVED durability is never a durably
        // completed transition — the crossing fails closed (no
        // further authoritative T1 writes: the pre-rotation journal
        // is a superset only of the evidence existing at rotation
        // time, never of future writes to the new journal) until
        // the durability is re-established (the R5 repair) or a
        // verified recovery repairs it.
        if let PointerDurability::Unresolved { .. } = rotation.pointer_durability {
            self.journal_authority = JournalAuthority::DurabilityUnresolved;
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "rotation/pointer-durability",
            });
        }
        // (F4-R1) THE REBUILD'S BOUND IDENTITIES: the crossing's own
        // post-flip probe (the same chain-verified pair the
        // session-bound entry used for the arm selection — the
        // rebuild opens the successor through the bound form when
        // the campaign is bound).
        let bound_identities = {
            let mut probe = self.source.open_medium().map_err(PipelineError::Medium)?;
            let founding = engine_persist::reclaim::founding_identity_medium(&mut probe)
                .map_err(PipelineError::Medium)?;
            engine_persist::reclaim::find_installed_journal_root_binding(&mut probe, &founding)
                .map_err(PipelineError::Medium)?
                .map(|(_id, binding)| (binding.founding, binding.root_id))
        };
        // THE SWAP: this crossing's pipeline reopens over the new
        // journal — the keys reseeded from it, the confirmed set
        // re-derived from the chain. THE POST-FLIP FAILURE RULE
        // (the review corrective R4): the pointer now names the new
        // journal, so a failure here must NEVER return control to an
        // operational stale-journal writer — the crossing is
        // explicitly unusable for further authoritative progression
        // (every mutation refuses) until a fresh recovery re-derives
        // it from the pointer.
        match self.rebuild_pipeline_after_rotation(
            &rotation.new_path,
            bound_identities.as_ref().map(|(f, r)| (f, r)),
        ) {
            Ok(()) => Ok(rotation),
            Err(error) => {
                self.journal_authority = JournalAuthority::Lost;
                Err(error)
            }
        }
    }

    /// THE R5 REPAIR: re-establish the pointer's power-loss
    /// durability (the directory sync) and, with it proven, rebuild
    /// this crossing's pipeline over the pointer-resolved
    /// authoritative journal — the crossing reopens for
    /// authoritative progression. Valid only in the
    /// durability-unresolved state (idempotent when healthy; the
    /// R4-lost state is unrecoverable in-process); a re-sync failure
    /// leaves the fail-closed state intact; a rebuild failure
    /// escalates to the R4 authority-lost state.
    pub fn reestablish_pointer_durability(&mut self) -> Result<(), TransitionError> {
        match self.journal_authority {
            JournalAuthority::Healthy => return Ok(()),
            JournalAuthority::Lost => {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "rotation/authority-lost",
                });
            }
            JournalAuthority::DurabilityUnresolved => {}
        }
        engine_persist::reestablish_pointer_durability(&self.journal_root)
            .map_err(PipelineError::Journal)?;
        let active = active_journal_path(&self.journal_root).map_err(PipelineError::Journal)?;
        // (F4-R10) THE AUTHORITY PROBE — ERRORS PROPAGATE, NEVER THE
        // LEGACY DOWNGRADE: the founding discovery and the installed
        // binding's read/verification failures are FAILURES, not the
        // authenticated absence of a binding. A failed probe must
        // never select the legacy journal open (`T1Journal::open`)
        // as a fallback — a bound campaign reopens through the bound
        // journal path or the repair refuses (the crossing stays in
        // its fail-closed state; `JournalAuthority::Healthy` is
        // restored only by a fully verified successful repair).
        let repair_bound = {
            let mut probe = self.source.open_medium().map_err(PipelineError::Medium)?;
            let founding = engine_persist::reclaim::founding_identity_medium(&mut probe)
                .map_err(PipelineError::Medium)?;
            engine_persist::reclaim::find_installed_journal_root_binding(&mut probe, &founding)
                .map_err(PipelineError::Medium)?
                .map(|(_id, binding)| (binding.founding, binding.root_id))
        };
        if let Err(error) = self
            .rebuild_pipeline_after_rotation(&active, repair_bound.as_ref().map(|(f, r)| (f, r)))
        {
            self.journal_authority = JournalAuthority::Lost;
            return Err(error);
        }
        self.journal_authority = JournalAuthority::Healthy;
        Ok(())
    }

    /// THE POST-FLIP SWAP (the review corrective R4): rebuild this
    /// crossing's pipeline over the rotation's new journal — called
    /// only after the authoritative pointer names it. A failure
    /// poisons the crossing (see [`SessionCrossing::rotate_journal`]).
    fn rebuild_pipeline_after_rotation(
        &mut self,
        new_path: &Path,
        bound: Option<(&engine_persist::Hash, &engine_persist::Hash)>,
    ) -> Result<(), TransitionError> {
        if ROTATION_SWAP_FAULT.load(Ordering::Relaxed) {
            return Err(TransitionError::JournalFailed(PipelineError::Medium(
                MediumError::Io {
                    op: "rotation swap (injected fault)",
                    path: new_path.display().to_string(),
                    source: std::io::Error::other("injected pipeline-rebuild failure"),
                },
            )));
        }
        let journal = match bound {
            Some((founding, root_id)) => T1Journal::open_bound(new_path, founding, root_id)
                .map_err(PipelineError::Journal)?,
            None => T1Journal::open(new_path).map_err(PipelineError::Journal)?,
        };
        let medium = self.source.open_medium().map_err(PipelineError::Medium)?;
        self.pipeline = CommitPipeline::open(medium, journal)?;
        Ok(())
    }

    /// The journal this crossing's pipeline holds (diagnostics — the
    /// rotation's swap and the recovery's pointer resolution are
    /// observable here).
    pub fn pipeline_journal_path(&self) -> &Path {
        self.pipeline.journal().path()
    }

    /// (F4 §6) The campaign's journal ROOT this crossing resolves
    /// rotations and publications from — the binding's authoritative
    /// location for a bound campaign, the supplied journal's path for
    /// a legacy one.
    pub fn journal_root_path(&self) -> &Path {
        &self.journal_root
    }

    /// The explicit two-phase reclamation (PP-B3, D-PP-4's
    /// verify-before-reclaim retention protocol — explicit, never
    /// automatic, never a silent threshold; the review correctives
    /// F2/F3/F4 incorporated). PHASE A (the eligibility + the audit
    /// — NO destructive action before the audit is durably
    /// established): THE RETENTION WINDOW IS VALIDATED FIRST (a
    /// positive `generations` — the recovery works from the newest
    /// SK, so a zero-generation window that could eventually delete
    /// every anchor is refused before anything); the compacted walk
    /// validates; THE PRE-DELETION ANCHOR INVARIANT verifies that
    /// the newest checkpoint's SK — never in any reclaim index — is
    /// present and carries a session-snapshot object (a verified,
    /// recoverable anchor remains after the deletion — never a
    /// post-delete check establishing a deletion prerequisite); THE
    /// TARGET SELECTION picks the oldest checkpoint beyond the
    /// window whose range is NOT fully reclaimed (MK records are
    /// permanent, so a completed reclamation is distinguished from
    /// restartable incomplete work — the reclamation progresses
    /// generation by generation, never repeating a completed
    /// range); the completed SK/MK binding verifies and the metadata
    /// decodes; the digest is RECOMPUTED against the still-present
    /// range bytes (the eligibility proof that what the index names
    /// is exactly what was walked and hashed) while THE COMPLETE
    /// DELETION MANIFEST — one unit per indexed record with its
    /// referenced content objects — is collected while the bytes
    /// are present; and the audit revision (a
    /// reclamation-ledger/1 object keyed deterministically by the
    /// reclaimed MK, carrying the manifest) is appended and read
    /// back verified. PHASE B (the deletes — idempotent +
    /// restartable, deleting from the AUDITED manifest, never a
    /// re-read of a possibly-absent record, so an interrupted
    /// unit's orphaned objects are discoverable and the retry
    /// converges): each unit's record object and its content
    /// objects removed together, quarantined copies removed from
    /// the quarantine, and the stale journal generations (each with
    /// its sidecar lock) removed only when the rotation's verified
    /// active journal stands (the survival set). The
    /// post-conditions verify: the compacted walk validates over
    /// the gapped chain (the skip tables cover the gaps), the
    /// membership universe carries the reclaimed identities and
    /// keys, and the newest snapshot (the recovery anchor) remains
    /// discoverable. NEVER the founding, the confirming anchors,
    /// the MK records and their metadata (all permanent), or the
    /// retained window's snapshots; bytes are never rewritten.
    pub fn reclaim_checkpoint(
        &mut self,
        legacy_journal: &Path,
        generations: u64,
    ) -> Result<ReclamationReport, TransitionError> {
        self.assert_journal_authority()?;
        // THE CAMPAIGN-ROOT BINDING (the review corrective R3): the
        // reclamation's journal retirement authority is THIS
        // crossing's campaign journal root — the path the crossing
        // opened or recovered with — never an arbitrary caller path
        // (a shared directory's foreign root must never direct this
        // campaign's deletions).
        if legacy_journal != self.journal_root.as_path() {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "reclaim/journal-root",
            });
        }
        // PHASE A — the eligibility + the audit. THE WINDOW
        // VALIDATION first (the review corrective F2): the retention
        // window must be positive — a zero-generation window could
        // eventually authorize deleting the newest SK, the only
        // recovery anchor. Refused BEFORE anything.
        if generations < 1 {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "reclaim/invalid-window",
            });
        }
        let mut medium = self.source.open_medium().map_err(PipelineError::Medium)?;
        let summary = medium.validate().map_err(PipelineError::Medium)?;
        // The retention window: the recovery works from the newest
        // SK — the `generations` newest checkpoints must remain.
        let Some(newest_mk) = summary.checkpoints.first().copied() else {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "reclaim/no-checkpoint",
            });
        };
        if summary.checkpoints.len() as u64 <= generations {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "reclaim/retention-window",
            });
        }
        // THE PRE-DELETION ANCHOR INVARIANT (the review corrective
        // F2): the newest checkpoint's SK — never in any reclaim
        // index — must be present and carry a session-snapshot
        // object BEFORE anything: a verified, recoverable anchor
        // remains after the deletion.
        {
            let newest_record = medium
                .read_record(&newest_mk)
                .map_err(PipelineError::Medium)?;
            let Some(anchor_sk) = newest_record.parent else {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "reclaim/mk-unfounded",
                });
            };
            let sk_record = medium
                .read_record(&anchor_sk)
                .map_err(PipelineError::Medium)?;
            let Some(anchor_object) = sk_record.content_refs.first() else {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "reclaim/anchor-not-snapshot",
                });
            };
            let anchor_bytes = medium
                .read_object(anchor_object)
                .map_err(PipelineError::Medium)?;
            if !is_session_object(&anchor_bytes) {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "reclaim/anchor-not-snapshot",
                });
            }
        }
        // THE TARGET SELECTION (the review correctives F3 + R1): the
        // oldest checkpoint beyond the window with manifest-authorized
        // work remaining. MK records are permanent — a completed
        // reclamation stays in the summary forever — so the selection
        // distinguishes three states: an UNAUDITED range with present
        // indexed records (the fresh eligibility, proven against the
        // still-present bytes below); an AUDITED range whose manifest
        // is NOT yet satisfied (an interrupted Phase B — resumed,
        // even when every indexed record file is already gone: the
        // referenced objects, the sweep-quarantined copies, or the
        // audited stale journal generations may remain); and an
        // AUDITED range whose manifest IS fully satisfied (completed
        // — skipped, never repeated).
        let mut target: Option<ReclaimTarget> = None;
        // (R4-L1) A marker whose historical attribution is mid-flight:
        // re-verified at the pass's end (a later retirement's
        // completion within this pass may resolve it).
        let mut historical: Vec<(Hash, Hash)> = Vec::new();
        for candidate in summary.checkpoints[generations as usize..].iter().rev() {
            let mk_record = medium
                .read_record(candidate)
                .map_err(PipelineError::Medium)?;
            let Some(sk) = mk_record.parent else {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "reclaim/mk-unfounded",
                });
            };
            let Some(metadata_object) = mk_record.content_refs.first() else {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "reclaim/mk-no-metadata",
                });
            };
            let metadata_bytes = medium
                .read_object(metadata_object)
                .map_err(PipelineError::Medium)?;
            let metadata =
                CheckpointMetadata::decode(&metadata_bytes).map_err(PipelineError::Format)?;
            if metadata.sk != sk {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "reclaim/metadata-binding",
                });
            }
            match find_installed_reclamation(&mut medium, candidate)? {
                Some((record_id, ledger)) => {
                    // THE AUDIT-COHERENCE verification (fail closed on
                    // invalid bindings — the resume standard): the
                    // bindings AND the manifest must cover exactly the
                    // permanent index's identities.
                    if ledger.sk != sk
                        || ledger.range_digest != metadata.range_digest
                        || ledger.records != metadata.range_records
                    {
                        return Err(TransitionError::InstallIncompatible {
                            region: 0,
                            detail: "reclaim/audit-incoherent",
                        });
                    }
                    let index_hashes: BTreeSet<Hash> =
                        metadata.index.iter().map(|entry| entry.hash).collect();
                    let unit_hashes: BTreeSet<Hash> =
                        ledger.units.iter().map(|unit| unit.record).collect();
                    if index_hashes != unit_hashes {
                        return Err(TransitionError::InstallIncompatible {
                            region: 0,
                            detail: "reclaim/audit-incoherent",
                        });
                    }
                    // THE WHOLE-MANIFEST COMPLETION CHECK (the review
                    // corrective R1): completion is judged against the
                    // ENTIRE durable audit — the medium's unit bytes
                    // (the record files, the referenced objects, the
                    // sweep-quarantined copies) AND the audited stale
                    // journal generations with their sidecar locks.
                    // Any remaining byte is restartable work; only a
                    // fully satisfied manifest is complete.
                    let journal_dir = legacy_journal.parent().unwrap_or_else(|| Path::new("."));
                    let journals_pending = ledger.journal_files.iter().any(|name| {
                        journal_dir.join(name).is_file()
                            || journal_dir.join(format!("{name}.lock")).is_file()
                    });
                    if medium
                        .reclaim_pending(&engine_persist::ReclaimDirective {
                            audit_record: record_id,
                            units: ledger.units.clone(),
                        })
                        .map_err(PipelineError::Medium)?
                        || journals_pending
                    {
                        target = Some((*candidate, sk, metadata, Some((record_id, ledger))));
                        break;
                    }
                    // Completed — the manifest fully satisfied. P2-F4:
                    // the terminal evidence is ensured BEFORE the
                    // skip — the restart after a completed deletion
                    // but before the marker's installation converges
                    // HERE (the same verified-completion admission,
                    // the state re-verified at publication), and an
                    // ALREADY-installed marker is verified through
                    // the trusted consumer (a marker-shaped object
                    // without completion authority is rejected —
                    // never silently honored).
                    match engine_persist::reclaim::find_installed_completion(&mut medium, candidate)
                        .map_err(PipelineError::Medium)?
                    {
                        Some((marker_id, _marker)) => {
                            if let Err(error) = engine_persist::reclaim::verify_reclamation_complete(
                                &mut medium,
                                &marker_id,
                                candidate,
                            ) {
                                let accounting_class = matches!(
                                    error,
                                    MediumError::Chain {
                                        rule: "reclamation-complete/mandatory-count-mismatch"
                                    } | MediumError::Chain {
                                        rule: "reclamation-complete/deferred-mismatch"
                                    } | MediumError::Chain {
                                        rule: "reclamation-complete/later-marker-incoherent"
                                    }
                                );
                                if !accounting_class {
                                    return Err(PipelineError::Medium(error).into());
                                }
                                // (R4-L1) The historical-attribution
                                // intermediate state: a LATER
                                // retirement's completion — possibly
                                // ensured later in THIS pass — may
                                // resolve this marker's accounting.
                                // Continue the scan; the deferred
                                // re-verification below fails closed
                                // when nothing resolves it. This is
                                // never a silent acceptance.
                                historical.push((marker_id, *candidate));
                            }
                        }
                        None => {
                            engine_persist::reclaim::complete_reclamation(
                                &mut medium,
                                &record_id,
                                candidate,
                            )
                            .map_err(PipelineError::Medium)?;
                        }
                    }
                    // Completed — skip.
                }
                None => {
                    // The fresh-eligibility probe: record presence.
                    let mut any_present = false;
                    for entry in &metadata.index {
                        match medium.read_record(&entry.hash) {
                            Ok(_) => {
                                any_present = true;
                                break;
                            }
                            Err(MediumError::MissingObject { .. }) => {}
                            Err(other) => return Err(PipelineError::Medium(other).into()),
                        }
                    }
                    if any_present {
                        target = Some((*candidate, sk, metadata, None));
                        break;
                    }
                }
            }
        }
        let Some((oldest_mk, sk, metadata, installed)) = target else {
            // (R4-L1) The deferred historical re-verification: a
            // later retirement's marker ensured during THIS pass may
            // have resolved the accounting; a still-failing marker is
            // a genuine fail-closed state (never silently accepted).
            // (R4-L2) EVERY deferred historical certification is
            // revalidated independently; a still-failing marker is a
            // genuine fail-closed state — never a silent acceptance,
            // never a lost obligation.
            for (marker_id, historical_mk) in &historical {
                if let Err(retry) = engine_persist::reclaim::verify_reclamation_complete(
                    &mut medium,
                    marker_id,
                    historical_mk,
                ) {
                    return Err(PipelineError::Medium(retry).into());
                }
            }
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "reclaim/nothing-eligible",
            });
        };
        // THE PRE-SCAN: the installed audit — discovered and
        // coherence-verified by the selection above — means Phase A
        // already durably recorded the eligibility: the re-derivation
        // verifies the installed record, never re-requiring the (now
        // partially deleted) range bytes; no audit means the FULL
        // Phase A below.
        let (ledger_id, ledger) = match installed {
            Some((record_id, ledger)) => (record_id, ledger),
            None => {
                // THE FULL PHASE A: the digest RECOMPUTED against the
                // still-present range bytes — the pre-reclamation
                // verification — AND the complete deletion manifest
                // collected while the bytes are present (the review
                // corrective F4: an interrupted Phase B deletes from
                // the manifest, never a re-read of a possibly-absent
                // record). The digest covers the RANGE PUBLICATIONS
                // oldest-first (epoch order); SK's own retirement
                // entry is an index member but never a digest
                // member.
                let mut indexed: Vec<&ReclaimedIndexEntry> = metadata.index.iter().collect();
                indexed.sort_unstable_by_key(|entry| entry.epoch);
                let mut range_bytes = Vec::with_capacity(indexed.len());
                let mut units: Vec<ReclaimUnit> = Vec::with_capacity(indexed.len());
                // C1's deletion order: the manifest's units run
                // NEWEST-FIRST (descending epoch), while the digest
                // covers the range publications oldest-first. The
                // walk authorizes a gap only under a RETAINED
                // revision's skip entry (never an indexed member) —
                // so deleting from the range's TOP keeps every
                // interrupted-Phase-B prefix walk-validatable: the
                // gap's upper boundary is the MK's self-skip, then a
                // retained record's entry once the range's top falls.
                // Deleting oldest-first would strand a present
                // indexed member above a deleted one — the exact
                // hand-corruption shape the walk must fail closed
                // on — making the mid-unit crash window (the F4
                // convergence contract) unrecoverable.
                for entry in indexed.iter().rev() {
                    let record = medium
                        .read_record(&entry.hash)
                        .map_err(PipelineError::Medium)?;
                    if entry.hash != sk {
                        range_bytes.push(record.canonical_bytes());
                    }
                    units.push(ReclaimUnit {
                        record: entry.hash,
                        objects: record.content_refs.clone(),
                    });
                }
                // The digest's own order: oldest-first over the
                // collected publication bytes (the units above ran
                // newest-first).
                range_bytes.reverse();
                let recomputed = chain_range_digest(&metadata.seed, &range_bytes);
                if recomputed != metadata.range_digest {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "reclaim/digest-mismatch",
                    });
                }
                // The journal survival evidence: the stale journal
                // generations are removable only after the rotation's
                // verified active journal stands.
                let journal_files = reclaimable_journal_files(legacy_journal)?;
                // THE AUDIT REVISION — appended BEFORE any
                // destructive action, then read back verified.
                let ledger = ReclamationLedger {
                    mk: oldest_mk,
                    sk,
                    range_digest: metadata.range_digest,
                    seed: metadata.seed,
                    records: metadata.range_records,
                    reclaimed_keys: metadata
                        .index
                        .iter()
                        .filter(|entry| entry.key.is_some())
                        .count() as u64,
                    journal_files,
                    units,
                };
                let head = medium.head().map_err(PipelineError::Medium)?;
                let Some(head) = head else {
                    return Err(PipelineError::UnfoundedMedium.into());
                };
                let tick = medium
                    .read_record(&head)
                    .map_err(PipelineError::Medium)?
                    .tick
                    .saturating_add(1);
                let (candidate, outcome) = publish_reclamation_ledger(&mut medium, &ledger, tick)
                    .map_err(PipelineError::Medium)?;
                let ledger_id = match outcome {
                    AppendOutcome::Accepted { id, .. } => id,
                    AppendOutcome::RejectedStaleParent { .. } => {
                        return Err(TransitionError::InstallIncompatible {
                            region: 0,
                            detail: "reclaim/audit-stale",
                        });
                    }
                    AppendOutcome::Unknown { .. } => {
                        match medium
                            .reconcile_unknown(&candidate)
                            .map_err(PipelineError::Medium)?
                        {
                            Reconciliation::Installed { id } => id,
                            Reconciliation::NotInstalled { .. } => {
                                let (_, retry) =
                                    publish_reclamation_ledger(&mut medium, &ledger, tick)
                                        .map_err(PipelineError::Medium)?;
                                match retry {
                                    AppendOutcome::Accepted { id, .. } => id,
                                    _ => {
                                        return Err(TransitionError::InstallIncompatible {
                                            region: 0,
                                            detail: "reclaim/audit-unresolved",
                                        });
                                    }
                                }
                            }
                            Reconciliation::Unverifiable { .. } => {
                                return Err(TransitionError::InstallIncompatible {
                                    region: 0,
                                    detail: "reclaim/audit-unverifiable",
                                });
                            }
                        }
                    }
                };
                // THE READ-BACK verification of the fresh audit.
                let audit_record = medium
                    .read_record(&ledger_id)
                    .map_err(PipelineError::Medium)?;
                let Some(audit_object) = audit_record.content_refs.first() else {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "reclaim/audit-readback",
                    });
                };
                let audit_bytes = medium
                    .read_object(audit_object)
                    .map_err(PipelineError::Medium)?;
                let read_back =
                    ReclamationLedger::decode(&audit_bytes).map_err(PipelineError::Format)?;
                if read_back != ledger {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "reclaim/audit-readback",
                    });
                }
                (ledger_id, ledger)
            }
        };
        // THE AUDIT-COMMITTED/DELETES-UNCOMMITTED crash seam
        // (test-only — see [`ARM_RECLAIM_AFTER_AUDIT`], the review
        // corrective F4's real-kill evidence): the audit IS durably
        // on the chain; Phase B never runs — the restart resumes
        // from the installed audit and completes to convergence.
        reclaim_after_audit_seam();
        // PHASE B — the deletes: idempotent + restartable, driven by
        // the AUDITED MANIFEST (an interrupted unit's orphaned
        // objects are discoverable from the ledger — including
        // copies the v1.4 sweep quarantined).
        // (OC-C3 CP-2) THE RECLAIM-FIRST ROUTING: the provider's
        // execution gate is the authority — the begin-bound
        // destructive authorization is verified under the provider's
        // own coordination BEFORE any mutation. The DEFINITE
        // canonical begin absence (the exact typed rule) is the ONLY
        // fresh-admission trigger: the accepted OC-C2 trusted
        // admission through CP-1's trait interface (on a freshly
        // valid range it publishes; a degraded no-begin range FAILS
        // CLOSED at the first-execution verifier — the R1 gate).
        // Only a newly acknowledged Accepted publication with the
        // required truthful tier permits ONE bounded retry (the
        // provider-internal gate re-verifies); every other outcome —
        // ReconciledInstalled, DuplicateVerified,
        // ReconciledNotInstalled, a lost reply, an unresolved
        // transport failure — is NOT a newly acknowledged durable
        // publication: the pass fails closed (a subsequent
        // authorized pass rediscovers the installed begin through
        // the provider's own execution gate, independently
        // establishing the grade — never a second begin publication
        // to repair uncertainty; the deterministic MK-bound key is
        // the idempotency boundary). Every other execution refusal
        // (corrupt, mismatched, orphaned, unverifiable,
        // insufficient-tier, commit-uncertain) propagates — STOP.
        let directive = engine_persist::ReclaimDirective {
            audit_record: ledger_id,
            units: ledger.units.clone(),
        };
        let removed_objects = match medium.reclaim(&directive) {
            Ok(outcome) => outcome.removed,
            Err(engine_persist::MediumError::Chain {
                rule: "execution/begin-absent",
            }) => {
                // The one fresh-admission trigger.
                let tick = medium
                    .read_record(&ledger_id)
                    .map_err(PipelineError::Medium)?
                    .tick
                    .saturating_add(1);
                match medium
                    .begin_reclamation(&ledger_id, &oldest_mk, tick)
                    .map_err(PipelineError::Medium)?
                {
                    engine_persist::reclaim::BeginPublicationOutcome::Accepted { tier, .. }
                        if tier == engine_persist::SyncTier::TransactionalWalFull
                            || tier == engine_persist::SyncTier::FullPosixSequence =>
                    {
                        // The ONE bounded retry — the provider's
                        // execution gate re-verifies the begin under
                        // its own coordination.
                        medium
                            .reclaim(&directive)
                            .map_err(PipelineError::Medium)?
                            .removed
                    }
                    _other => {
                        // A membership-only, duplicate, absent, or
                        // otherwise unacknowledged publication is
                        // never treated as a newly acknowledged
                        // durable one: fail closed for this pass (a
                        // subsequent authorized pass rediscovers the
                        // installed begin through the provider's own
                        // execution gate — never a second
                        // publication).
                        return Err(TransitionError::InstallIncompatible {
                            region: 0,
                            detail: "reclaim/begin-publication-unacknowledged",
                        });
                    }
                }
            }
            Err(other) => return Err(PipelineError::Medium(other).into()),
        };
        // THE MEDIUM-COMMITTED/JOURNAL-RETIREMENT-UNCOMMITTED crash
        // seam (test-only — see [`ARM_RECLAIM_AFTER_MEDIUM`], the
        // review corrective R1's real-kill evidence): every
        // manifest-authorized medium byte is gone; the audited stale
        // journal generations never retire — the restart resumes
        // from the installed audit and completes the cleanup.
        reclaim_after_medium_seam();
        // The stale journal generations (only those named in the
        // audited ledger — only after the rotation's verified active
        // journal stands), each with its sidecar lock.
        let mut removed_journals = Vec::new();
        for name in &ledger.journal_files {
            let dir = legacy_journal.parent().unwrap_or_else(|| Path::new("."));
            let path = dir.join(name);
            if path.is_file() {
                std::fs::remove_file(&path).map_err(|error| {
                    TransitionError::JournalFailed(PipelineError::Medium(MediumError::Io {
                        op: "reclaim journal file",
                        path: path.display().to_string(),
                        source: error,
                    }))
                })?;
                removed_journals.push(path);
            }
            let lock = dir.join(format!("{name}.lock"));
            if lock.is_file() {
                std::fs::remove_file(&lock).map_err(|error| {
                    TransitionError::JournalFailed(PipelineError::Medium(MediumError::Io {
                        op: "reclaim journal lock",
                        path: lock.display().to_string(),
                        source: error,
                    }))
                })?;
            }
        }
        // The ownership record updated (the review corrective R3):
        // the retired names leave the campaign's lineage (the
        // manifest rewritten atomically — dead entries are inert
        // either way; the record stays honest).
        retire_journal_generations(legacy_journal, &ledger.journal_files)
            .map_err(PipelineError::Journal)?;
        // THE POST-CONDITIONS: the compacted walk validates over the
        // gapped chain (the skip tables cover the gaps), and the
        // newest snapshot — the recovery anchor, the retention
        // window's whole point — remains discoverable.
        let summary = medium.validate().map_err(PipelineError::Medium)?;
        if summary.reclaimed_records < metadata.range_records {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "reclaim/post-walk",
            });
        }
        if latest_snapshot_at(&self.source)?.is_none() {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "reclaim/anchor-lost",
            });
        }
        // P2-F4: THE TRUSTED COMPLETION RECEIPT — the marker
        // installed through the verified-completion admission (the
        // postconditions re-verified against the live medium at
        // publication; the CAS/reconciliation/duplicate handling
        // inside). The terminal evidence maintenance discovery
        // recognizes: a completed v1 retirement never reselects as
        // unfinished work.
        // (test-only — see [`ARM_RECLAIM_BEFORE_COMPLETION`], the
        // R4-L1 real-death evidence): the process dies AFTER the
        // retirement's full destructive work and BEFORE its
        // completion marker — the mid-flight historical state an
        // EARLIER marker's verification must converge through.
        #[cfg(feature = "crash-test-seams")]
        reclaim_before_completion_seam();
        let _completion =
            engine_persist::reclaim::complete_reclamation(&mut medium, &ledger_id, &oldest_mk)
                .map_err(PipelineError::Medium)?;
        // (R4-L2) The maintenance boundary: the deferred obligations
        // survive the early target selection — revalidated after
        // this retirement's completion (its own marker may have
        // resolved them); a still-failing marker fails closed (the
        // report never falsely implies all encountered historical
        // evidence was verified).
        for (marker_id, historical_mk) in &historical {
            if let Err(retry) = engine_persist::reclaim::verify_reclamation_complete(
                &mut medium,
                marker_id,
                historical_mk,
            ) {
                return Err(PipelineError::Medium(retry).into());
            }
        }
        Ok(ReclamationReport {
            ledger: ledger_id,
            mk: oldest_mk,
            sk,
            eligible_records: metadata.range_records,
            removed_objects,
            journal_files: removed_journals,
        })
    }

    /// The canonical re-drive: batches the actionable unconfirmed
    /// journal keys into a consequence-batch/2 revision on the chain
    /// (the pending-T1 → confirmed-canonical transition — the gating
    /// demonstration's other side).
    pub fn commit_canonical(&mut self) -> Result<CommitOutcome, TransitionError> {
        self.assert_journal_authority()?;
        Ok(self.pipeline.commit_canonical()?)
    }

    /// The provider-aware recovery (PP-B1, source-generalized at
    /// PP-B4 checkpoint 1): the same reconstruction over the explicit
    /// medium source — the same composition, the same contracts, the
    /// same fail-closed behavior.
    pub fn recover_at(
        source: &S,
        journal_path: &Path,
        campaign: &str,
    ) -> Result<(Spine<NpcWorld>, RegionDirector, Self), TransitionError> {
        // 1. The snapshot — fail closed on a recognizable-but-invalid
        //    newest session object (corruption of the newest
        //    authoritative state is never walked past).
        let Some((mut spine, mut director, snapshot_content, snapshot_revision)) =
            latest_snapshot_at(source)?
        else {
            return Err(TransitionError::NoSnapshot);
        };
        // The snapshot-origin materialization evidence, captured
        // BEFORE any recovery application mutates the world: the
        // verified scratch the first post-snapshot capture consumed.
        let scratch_at_snapshot: Vec<u64> = spine.world().pending_signals().to_vec();
        // 2. The journal scan + the actionability filter: a durably
        //    canceled transaction is never replayed. A journal file
        //    that no longer exists (the historical T1 intents dropped
        //    after canonical confirmation — the accepted B3 coherence
        //    case: a confirmed consequence materialized in a verified
        //    snapshot needs no historical T1) is an EMPTY scan: the
        //    chain and the snapshot carry the truth. THE
        //    active-journal resolution (PP-B3): the rotation's
        //    pointer beside the legacy path names the rotated
        //    journal; a missing pointer resolves the legacy itself —
        //    every pre-rotation campaign recovers identically.
        let active_journal = active_journal_path(journal_path).map_err(PipelineError::Journal)?;
        // THE DURABILITY VERIFICATION (the review corrective R5):
        // the recovery never proceeds on a possibly-unsynced
        // pointer — a fresh recovery cannot silently clear an
        // unresolved durability-uncertain condition merely by
        // reading the currently visible pointer; the directory sync
        // is completed here (or the recovery fails closed) before
        // any progression. On platforms where std exposes no
        // directory flush (Windows) this is the ratified no-op.
        engine_persist::reestablish_pointer_durability(journal_path)
            .map_err(PipelineError::Journal)?;
        // A POINTER-RESOLVED journal that does not exist is a broken
        // rotation state — fail closed, never the empty fallback. The
        // absent-LEGACY case (no pointer present) keeps the accepted
        // B3 coherence semantics: an empty scan, the chain and the
        // snapshot carrying the truth.
        let pointer_present = active_journal != journal_path;
        // (V2-W F4) THE BOUND RECOVERY'S PROBE — BEFORE THE SCAN: a
        // campaign with an installed journal-root binding recovers
        // ONLY through the bound identities — the binding's location
        // must equal the supplied root's normalized location (a
        // foreign or relocated root refuses), and the binding
        // SELECTS THE SCANNER: a bound campaign's active journal
        // carries the versioned bound header, which the legacy
        // scanner refuses (the domain-tag mismatch) — the bound
        // identity-verified scan replaces it (the active journal
        // must carry the EXACT founding hash and root ID; a legacy,
        // foreign, or corrupt header refuses; a missing active is
        // MISSING authoritative evidence, fail closed — never the
        // empty fallback). A campaign with NO binding keeps the
        // accepted legacy recovery unchanged (the no-migration
        // rule).
        let mut probe = source.open_medium().map_err(PipelineError::Medium)?;
        let founding = engine_persist::reclaim::founding_identity_medium(&mut probe)
            .map_err(PipelineError::Medium)?;
        let bound_identities = match engine_persist::reclaim::find_installed_journal_root_binding(
            &mut probe, &founding,
        )
        .map_err(PipelineError::Medium)?
        {
            Some((_id, binding)) => {
                let location =
                    engine_persist::reclaim::JournalRootBinding::normalize_location(journal_path);
                if binding.location != location {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "journal-root/location-mismatch",
                    });
                }
                Some((binding.founding, binding.root_id))
            }
            None => None,
        };
        drop(probe);
        let scan = match &bound_identities {
            Some((founding, root_id)) => {
                engine_persist::journal::scan_bound_file(&active_journal, founding, root_id)
                    .map_err(|error| TransitionError::InstallIncompatible {
                        region: 0,
                        detail: match &error {
                            engine_persist::JournalError::Io { source, .. }
                                if source.kind() == std::io::ErrorKind::NotFound =>
                            {
                                "journal-root/missing-journal"
                            }
                            _ => "journal-root/active-corrupt",
                        },
                    })?
            }
            None => match T1Journal::scan_file(&active_journal) {
                Ok(scan) => scan,
                Err(engine_persist::JournalError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound && !pointer_present =>
                {
                    JournalScan {
                        committed: Vec::new(),
                        cancellations: Vec::new(),
                        tail: TailState::Clean,
                    }
                }
                Err(error) => return Err(PipelineError::Journal(error).into()),
            },
        };
        let canceled: HashSet<engine_persist::TransactionId> =
            scan.cancellations.iter().map(|c| c.target).collect();
        // 3. The interleaved crossing restore + plan install — the
        //    actionable, post-snapshot deactivations in
        //    journal-sequence order, over the re-opened crossing.
        //    `restored` tracks each NPC's first post-snapshot claim:
        //    the first subtracts the snapshot's scratch (the
        //    crossings the snapshot already materialized are NEVER
        //    re-applied), every later one claims against zero — the
        //    prior install's capture zeroed the scratch exactly as
        //    the live sequence did.
        let journal = match bound_identities {
            Some((founding, root_id)) => {
                T1Journal::open_bound(&active_journal, &founding, &root_id)
                    .map_err(PipelineError::Journal)?
            }
            None => T1Journal::open(&active_journal).map_err(PipelineError::Journal)?,
        };
        let mut crossing = Self::open_at(source, journal, campaign)?;
        // The campaign journal ROOT (the review corrective F1): the
        // pointer's authoritative location — NOT the resolved active
        // journal the pipeline holds. Repeated rotations and the
        // recovery both resolve from this one root.
        crossing.journal_root = journal_path.to_path_buf();
        // THE C1 COVERING TRACK'S RECOVERY SEED: the restored
        // snapshot is an ACCEPTED, validated snapshot of the exact
        // state the recovery begins from — any nonempty plan prepared
        // from the UNMUTATED recovered state (no replay installs) is
        // covered by it. The replay's installs mutate the state, so
        // post-replay preparations serialize differently and publish
        // fresh coverings — the equivalence check decides.
        crossing.covering = Some(CoveringSnapshot {
            content_hash: snapshot_content,
            revision: snapshot_revision,
        });
        let mut restored = vec![false; NPC_COUNT];
        // THE RECOVERED SNAPSHOT'S ALLOCATION FRONTIER (the S6-C4
        // stage 1-A frontier guard's input): the restored spine
        // passed `Spine::restore()`'s dense-allocation gate —
        // `stats.scheduled == next_seq` — so on a RESTORED spine the
        // scheduled count IS the allocation frontier (the
        // equivalence established by the restore's own integrity
        // contract, never assumed elsewhere). Captured ONCE, BEFORE
        // the replay mutates anything: every identity the durable
        // evidence can coherently account for lies strictly below
        // this bound.
        let allocation_frontier = spine.stats().scheduled;
        for transaction in &scan.committed {
            if canceled.contains(&transaction.id) {
                continue;
            }
            if transaction.plan.is_empty() {
                // A foreign (plan-less) claim: the application recovery
                // cannot reconstruct its world effects — the anchor
                // below surfaces it as unaccounted (the halt), never a
                // silent partial restore.
                continue;
            }
            let plan =
                DeactivationPlan::restore(&transaction.plan).map_err(PipelineError::Format)?;
            if plan.episode <= director.states()[plan.region].episode {
                // A pre-snapshot deactivation: already materialized in
                // the snapshot's ledger and world — never re-applied.
                continue;
            }
            // THE ALLOCATION-FRONTIER PREFLIGHT (the S6-C4 stage 1-A
            // repeated-recovery blocker's fail-closed guard): the
            // plan's obligation and dependency identities were
            // allocated by the LOST session; the recovered snapshot's
            // allocation frontier bounds every identity the durable
            // snapshot-plus-journal evidence can coherently account
            // for. A plan referencing identities at or beyond that
            // frontier carries obligations whose allocation and
            // execution history was never durably saved —
            // reconstructing them would produce a state the strict
            // save/restore contract refuses on the next reopen (the
            // measured failure: reconstructed obligations at seqs
            // 21380..21786 against a frontier of 1000). Refuse
            // BEFORE any inferred reclassification, reactivation, or
            // install — a typed, distinguishable error; the durable
            // campaign state is untouched; no renumbering, no
            // invented history. This guard is a necessary
            // precondition, not proof that reconstruction is
            // otherwise correct — the final reconstructed-state
            // validation below still runs for everything that passes.
            for obligation in &plan.obligations {
                if obligation.id.seq >= allocation_frontier {
                    return Err(TransitionError::RecoveryInsufficient {
                        region: plan.region,
                        identity: obligation.id.seq,
                        frontier: allocation_frontier,
                    });
                }
                for dep in &obligation.depends_on {
                    if dep.seq >= allocation_frontier {
                        return Err(TransitionError::RecoveryInsufficient {
                            region: plan.region,
                            identity: dep.seq,
                            frontier: allocation_frontier,
                        });
                    }
                }
            }
            // The implicit reactivation between episodes, DERIVED from
            // this plan's own from-level (the durable evidence the
            // region was live at its deactivation): a prior episode's
            // install left the region dormant, so the reactivation —
            // the legacy activation install plus the live application
            // of the prior episode's captures — is part of the honest
            // reconstruction, replayed through the real activation
            // path at the plan's own tick (nothing new journaled; the
            // application is exactly the one the lost session
            // performed).
            if director.levels()[plan.region] == LodLevel::Dormant
                && plan.from_level != LodLevel::Dormant
            {
                crossing.activate(
                    &mut spine,
                    &mut director,
                    plan.region,
                    plan.from_level,
                    plan.tick,
                )?;
            }
            // THE MISSING LIVE-TO-LIVE RECLASSIFICATION (the S6-C4
            // stage 1-A interrupted-run corrective): the snapshot's
            // level for the region is a LIVE level that differs from
            // the plan's journal-verified `from_level` — the lost
            // session reclassified the region between the snapshot
            // and this deactivation through the director's own
            // active-to-active transition, which is deliberately
            // unjournaled and unpublished (only dormant crossings are
            // durable). The plan's `from_level` is the durable
            // evidence of the level AT the deactivation — the same
            // evidence the dormant case above already trusts — so the
            // replay reconstructs the reclassification through the
            // SAME machinery the live session used, BEFORE the
            // install: a level-and-scheduling-lane change only. No
            // activation, checkpoint, capture, ledger entry, or
            // consequence is invented (the region was never dormant;
            // there is nothing to reactivate). The episode
            // relationship is validated FIRST — a plan skipping
            // episodes is genuinely inconsistent state and fails
            // closed HERE, before any inferred mutation; the
            // install's own preconditions (level, episode, ledger
            // keys, scratch) then run unchanged over the releveled
            // state, and the subsequent identity-exact
            // `reconstruct_held` replacement rebuilds the region's
            // obligations from the plan's own data, so the
            // intermediate lane change never alters the reconstructed
            // identities, conservation, or consequence
            // materialization.
            let current_level = director.levels()[plan.region];
            if current_level != LodLevel::Dormant
                && plan.from_level != LodLevel::Dormant
                && current_level != plan.from_level
            {
                if director.states()[plan.region].episode + 1 != plan.episode {
                    return Err(TransitionError::InstallIncompatible {
                        region: plan.region,
                        detail: "episode-advanced",
                    });
                }
                director.transition_region(&mut spine, plan.region, plan.from_level, plan.tick);
            }
            for envelope in &transaction.envelopes {
                let npc = envelope.id.npc as usize;
                let base = if restored[npc] {
                    0
                } else {
                    scratch_at_snapshot[npc]
                };
                let missing = envelope.delta.milestones.saturating_sub(base);
                if missing > 0 {
                    spine.world_mut().apply_recovered_crossings(npc, missing);
                }
                restored[npc] = true;
            }
            install_recovered_deactivation(&mut spine, &mut director, &plan)?;
        }
        // 4. The coherence verification — the REAL canonical-chain
        //    confirmation evidence from the re-opened pipeline (never
        //    an empty map), and the anchor built from the LEDGER's
        //    own per-identity state (journal-independent; a compacted
        //    snapshot's summarized coverage reconstructs from the
        //    same chain evidence under the lane boundary's
        //    attestation — PP-B2).
        let confirmed = crossing.pipeline.confirmed_evidence();
        let anchor = build_anchor(&spine, &director, &confirmed)?;
        let outcome = reconcile(&anchor, &scan, &confirmed).map_err(TransitionError::Recovery)?;
        if let Some(effect) = outcome.report.applied.first() {
            return Err(TransitionError::InstallIncompatible {
                region: effect.id.region as usize,
                detail: "recovery-unaccounted-claim",
            });
        }
        // The confirmed-recovered effects (the B3 coherence
        // contract's distinctly-reported exactly-once reconstruction
        // from the canonical evidence): in this application they are
        // exactly the PLAN-LESS foreign claims — confirmed
        // consequences with no deactivation plan, no checkpoint, no
        // activation path, and a foreign episode choice outside the
        // director's own frontier arithmetic. The crossing's
        // coherent continuation REQUIRES the monotonic
        // episode/identity frontier (the next ordinary deactivation
        // derives its identities from the director's episode
        // counter), so a restored plan-less capture under such an
        // identity would collide with the next derived identity
        // (the pipeline's ConflictingReuse) and strand the session
        // with a pending reward no activation can reach.
        // Materializing it would require exactly the fabrication the
        // recovery never does — an invented checkpoint, activation,
        // or source execution — so the application REJECTS the
        // unsupported plan-less form explicitly: fail closed, never
        // a successful recovery whose continuation is incoherent.
        if let Some(effect) = outcome.report.confirmed_recovered.first() {
            return Err(TransitionError::InstallIncompatible {
                region: effect.id.region as usize,
                detail: "confirmed-recovered-planless-unsupported",
            });
        }
        // 5a. THE FINAL RECONSTRUCTED-STATE VALIDATION (the
        // repeated-recovery blocker's second guard): the recovered
        // session must satisfy the engine's own strict
        // save/restore contract BEFORE any authoritative progression
        // or canonical publication — a reconstructed state the
        // decoder would refuse after publication is refused HERE,
        // never published, never a misleading successful recovery.
        // The roundtrip uses the existing serialization machinery
        // verbatim (no new canonical representation, no
        // counter-only shortcut, both history forms); a failure is
        // the typed validation error carrying the decoder's own
        // diagnostic — never a normalization, repair, or fabricated
        // history.
        let recovered_bytes = save_session(&spine, &director).map_err(PipelineError::Format)?;
        restore_session(&recovered_bytes).map_err(|error| TransitionError::RecoveryInvalid {
            detail: format!("{error:?}"),
        })?;
        // 5. The recovered session, with the re-opened crossing.
        Ok((spine, director, crossing))
    }
}

/// Test-only (the genuine mid-install crash seam): when armed, the
/// live install hard-exits BETWEEN the spine suspension and the
/// ledger capture — some install mutations applied, the rest never
/// run — the exact §5.5.2 mid-install termination window the
/// process-kill regression drives from a real child process (exit 7;
/// the pre-install gap's convention is 6). Engine code never arms
/// this; the static is process-local, so only the child that armed
/// it dies.
#[doc(hidden)]
pub static ARM_MID_INSTALL_EXIT: AtomicBool = AtomicBool::new(false);

/// The mid-install seam's exit point (see [`ARM_MID_INSTALL_EXIT`]).
fn mid_install_seam() {
    if ARM_MID_INSTALL_EXIT.load(Ordering::Relaxed) {
        std::process::exit(7);
    }
}

/// Test-only (the C1 covering-committed/T1-uncommitted crash seam,
/// the recovery-coverage checkpoint's mandatory window): when armed,
/// the crossing hard-exits AFTER the covering snapshot's acceptance
/// and BEFORE the T1 capture — the exact "after covering acceptance,
/// before T1 commit" window (exit 10, distinct from the mid-install
/// 7, the after-SK 8, and the abort seam's 9). A process death here
/// leaves the covering snapshot standing with NO journaled plan —
/// the recovery carries the pre-deactivation state through the
/// snapshot, the deactivation never happened durably. Engine code
/// never arms this; the static is process-local, so only the child
/// that armed it dies.
#[doc(hidden)]
pub static ARM_AFTER_COVERING_EXIT: AtomicBool = AtomicBool::new(false);

/// The after-covering seam's exit point (see
/// [`ARM_AFTER_COVERING_EXIT`]).
fn after_covering_seam() {
    if ARM_AFTER_COVERING_EXIT.load(Ordering::Relaxed) {
        std::process::exit(10);
    }
}

/// One covering-append attempt: the attempted record's identity (the
/// uncertain window's reconciliation anchor — preserved across the
/// append's own outcome OR error) and the append's result. The error
/// form carries the typed medium failure (the duplicate-idempotency
/// key rejection the prerequisite resolves against the chain — see
/// [`SessionCrossing::resolve_covering_append_error`]); the identity
/// must survive it, never be reconstructed.
struct CoveringAttempt {
    candidate: Hash,
    outcome: Result<AppendOutcome, MediumError>,
}

/// The C1 covering's single CAS append (see
/// [`SessionCrossing::establish_covering`]): the validated snapshot
/// object against the named parent — the same record construction
/// every snapshot publication uses (the content hash as the
/// idempotency key, the single content reference, the campaign
/// clock). Returns the attempted record's identity alongside the
/// outcome: the uncertain (`Unknown`) window's reconciliation is
/// BY IDENTITY, so the caller must reconcile exactly the record
/// this call built — never a reconstruction (an epoch or parent
/// mismatch would misclassify an installed covering as absent).
fn covering_append(
    medium: &mut dyn CanonicalMedium,
    object: &StoredObject,
    content_hash: Hash,
    parent: Hash,
    clock: Tick,
) -> Result<CoveringAttempt, PipelineError> {
    let parent_epoch = medium.read_record(&parent)?.epoch;
    let record = RevisionRecord::regular(parent, parent_epoch, clock.as_u64())
        .with_idempotency_key(content_hash.as_bytes().to_vec())
        .with_content_refs(vec![content_hash]);
    let candidate = record.id();
    Ok(CoveringAttempt {
        candidate,
        outcome: medium.append(&record, std::slice::from_ref(object), Some(parent)),
    })
}

/// Test-only (the genuine SK-committed/MK-uncommitted crash seam,
/// PP-B3's mandatory regression): when armed, the checkpoint
/// publication hard-exits AFTER SK commits but BEFORE MK — the exact
/// two-revision-composition boundary (exit 8, distinct from the
/// mid-install convention's 7). A process death here leaves the
/// ordinary compacted snapshot standing with NO checkpoint status
/// and NO reclaim eligibility — the recovery must find SK as an
/// ordinary publication. Engine code never arms this; the static is
/// process-local, so only the child that armed it dies.
#[doc(hidden)]
pub static ARM_AFTER_SK_EXIT: AtomicBool = AtomicBool::new(false);

/// The after-SK seam's exit point (see [`ARM_AFTER_SK_EXIT`]).
fn after_sk_seam() {
    if ARM_AFTER_SK_EXIT.load(Ordering::Relaxed) {
        std::process::exit(8);
    }
}

/// Test-only (the genuine audit-committed/deletes-uncommitted
/// reclamation crash seam, the review corrective F4's real-kill
/// evidence): when armed, the reclamation hard-exits AFTER the
/// audit revision is durably installed and read back verified and
/// BEFORE any Phase B deletion — the exact restart boundary (exit
/// 9, distinct from the after-SK seam's 8 and the mid-unit seam's
/// 10). A process death here leaves the audit on the chain and the
/// range intact: the re-run's pre-scan recognizes the installed
/// audit and resumes to completion. Engine code never arms this;
/// the static is process-local, so only the child that armed it
/// dies.
#[doc(hidden)]
pub static ARM_RECLAIM_AFTER_AUDIT: AtomicBool = AtomicBool::new(false);

/// The after-audit seam's exit point (see [`ARM_RECLAIM_AFTER_AUDIT`]).
fn reclaim_after_audit_seam() {
    if ARM_RECLAIM_AFTER_AUDIT.load(Ordering::Relaxed) {
        std::process::exit(9);
    }
}

/// Test-only (the genuine medium-committed/journal-retirement-
/// uncommitted reclamation crash seam, the review corrective R1's
/// real-kill evidence): when armed, the reclamation hard-exits AFTER
/// the canonical medium's manifest deletes complete and BEFORE the
/// audited stale journal generations retire — every indexed record
/// and object is gone while the manifest is NOT complete (the
/// journal files and their sidecar locks remain), the exact
/// post-medium window the whole-manifest completion probe closes
/// (exit 12, distinct from the after-audit seam's 9, the mid-unit
/// seam's 10, and the last-unit seam's 11). Engine code never arms
/// this; the static is process-local, so only the child that armed
/// it dies.
#[doc(hidden)]
pub static ARM_RECLAIM_AFTER_MEDIUM: AtomicBool = AtomicBool::new(false);

/// The after-medium seam's exit point (see [`ARM_RECLAIM_AFTER_MEDIUM`]).
fn reclaim_after_medium_seam() {
    if ARM_RECLAIM_AFTER_MEDIUM.load(Ordering::Relaxed) {
        std::process::exit(12);
    }
}

/// Test-only (the R4-L1 deterministic fault injection): when armed,
/// the process terminates AFTER the retirement's destructive work
/// (the medium reclamation, the journal retirement, the
/// postconditions) and BEFORE the completion marker's publication —
/// the exact mid-flight historical state (an EARLIER marker's
/// deferred object deleted by THIS retirement, no marker yet for
/// it) the crossing's autonomous recovery must converge through.
/// Engine code never arms this; the static is process-local.
/// COMPILE-TIME ISOLATED (the T2-B corrective): the seam exists
/// ONLY under the crash-test-seams feature — the production build
/// carries neither the arm nor the process-exit behavior (the CI's
/// feature-disabled artifact check enforces the absence).
#[cfg(feature = "crash-test-seams")]
#[doc(hidden)]
pub static ARM_RECLAIM_BEFORE_COMPLETION: AtomicBool = AtomicBool::new(false);

/// The before-completion seam's exit point (see
/// [`ARM_RECLAIM_BEFORE_COMPLETION`]).
#[cfg(feature = "crash-test-seams")]
fn reclaim_before_completion_seam() {
    if ARM_RECLAIM_BEFORE_COMPLETION.load(Ordering::Relaxed) {
        std::process::exit(13);
    }
}

/// Test-only (the review corrective R4's deterministic fault
/// injection): when armed, the post-flip pipeline rebuild fails
/// with a real I/O error — the post-flip failure that must poison
/// the crossing (the pointer names the new journal; the in-memory
/// writer must never continue through the stale one). Engine code
/// never arms this; the static is process-local.
#[doc(hidden)]
pub static ROTATION_SWAP_FAULT: AtomicBool = AtomicBool::new(false);

/// Phase 5 — the staged prepare/install bundle (§5.5.2): PREPARE
/// performs every fallible step and precondition re-validation
/// BEFORE any mutation (nothing is installed if a check fails); then
/// INSTALL is non-fallible by construction — removals and insertions
/// of already-validated values — with the one post-mutation check
/// (the suspension count) being the defensive-failure halt signal.
fn install_deactivation(
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    plan: &DeactivationPlan,
) -> Result<(), TransitionError> {
    let region = plan.region;
    // PREPARE — every fallible check before any mutation.
    if director.levels()[region] != plan.from_level {
        return Err(TransitionError::InstallIncompatible {
            region,
            detail: "level-moved",
        });
    }
    if director.states()[region].episode + 1 != plan.episode {
        return Err(TransitionError::InstallIncompatible {
            region,
            detail: "episode-advanced",
        });
    }
    let class = EventClass::new(region as u16);
    let present: HashSet<u64> = spine
        .collect_matching(|view| view.class == class)
        .iter()
        .map(|view| view.id.seq)
        .collect();
    for id in plan.ids() {
        if !present.contains(&id.seq) {
            return Err(TransitionError::InstallIncompatible {
                region,
                detail: "obligation-moved",
            });
        }
    }
    for (npc, _) in &plan.captures {
        let id = ConsequenceId::new(region as u32, plan.episode, *npc as u32);
        if !matches!(director.ledger().status(id), CaptureStatus::Unknown) {
            return Err(TransitionError::InstallIncompatible {
                region,
                detail: "ledger-key-exists",
            });
        }
    }
    for (npc, delta) in &plan.captures {
        if spine.world().pending_signals()[*npc] != *delta {
            return Err(TransitionError::InstallIncompatible {
                region,
                detail: "scratch-moved",
            });
        }
    }
    // INSTALL — non-fallible by construction (every value
    // pre-validated; the suspension count is the defensive halt).
    let ids = plan.ids();
    let suspended = spine.suspend_ids(&ids);
    if suspended != ids.len() as u32 {
        return Err(TransitionError::InstallIncompatible {
            region,
            detail: "short-suspension",
        });
    }
    // The genuine mid-install crash seam (test-only — see
    // [`ARM_MID_INSTALL_EXIT`]): the spine IS suspended, the ledger
    // NOT yet captured — a process death here leaves the journal's
    // durable intent standing and a partially-installed in-memory
    // state that dies with the process.
    mid_install_seam();
    director.install_captures(region, plan.episode, &plan.captures);
    spine.world_mut().set_region_lod(region, LodLevel::Dormant);
    spine.world_mut().discard_region_routine(region);
    director.install_deactivated(
        region,
        plan.episode,
        RegionCheckpoint {
            dormant_since: plan.tick,
            from_level: plan.from_level,
            entries: plan.entries.clone(),
        },
    );
    Ok(())
}

/// The plan's obligations as spine events (the id-exact
/// reconstruction's input): each under its OWN identity, with the
/// due carried as a logical-time schedule (the held entry's queue
/// priority derives from the due alone; the resumed follow-ups
/// create their own execution-relative schedules).
fn plan_events(plan: &DeactivationPlan) -> Vec<Event> {
    plan.obligations
        .iter()
        .map(|obligation| Event {
            id: obligation.id,
            class: obligation.class,
            schedule: obligation.schedule,
            payload: obligation.payload,
            field: obligation.field,
            depends_on: obligation.depends_on.clone(),
        })
        .collect()
}

/// The recovery's phase-5 installer (§5.5.1 case 6 — the id-exact
/// held reconstruction on the load path): the same staged
/// precondition re-validation as the live install EXCEPT the
/// obligation-presence check — the recovered spine's chains are the
/// SNAPSHOT's, and the plan's ids are the JOURNALED chains (moved by
/// the post-snapshot executions the crash lost) — so the install
/// REPLACES the region's stale chains with the plan's obligation data
/// through [`Spine::reconstruct_held`], never a live suspension.
fn install_recovered_deactivation(
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    plan: &DeactivationPlan,
) -> Result<(), TransitionError> {
    let region = plan.region;
    // PREPARE — the level, episode, ledger, and scratch checks (the
    // same standard; the crossing restore made the scratch match the
    // captured deltas).
    if director.levels()[region] != plan.from_level {
        return Err(TransitionError::InstallIncompatible {
            region,
            detail: "level-moved",
        });
    }
    if director.states()[region].episode + 1 != plan.episode {
        return Err(TransitionError::InstallIncompatible {
            region,
            detail: "episode-advanced",
        });
    }
    for (npc, _) in &plan.captures {
        let id = ConsequenceId::new(region as u32, plan.episode, *npc as u32);
        if !matches!(director.ledger().status(id), CaptureStatus::Unknown) {
            return Err(TransitionError::InstallIncompatible {
                region,
                detail: "ledger-key-exists",
            });
        }
    }
    for (npc, delta) in &plan.captures {
        if spine.world().pending_signals()[*npc] != *delta {
            return Err(TransitionError::InstallIncompatible {
                region,
                detail: "scratch-moved",
            });
        }
    }
    // INSTALL — the id-exact held reconstruction (the stale chains
    // replaced by the plan's), then the same state writes.
    let class = EventClass::new(region as u16);
    spine.reconstruct_held(|view| view.class == class, plan_events(plan));
    director.install_captures(region, plan.episode, &plan.captures);
    spine.world_mut().set_region_lod(region, LodLevel::Dormant);
    spine.world_mut().discard_region_routine(region);
    director.install_deactivated(
        region,
        plan.episode,
        RegionCheckpoint {
            dormant_since: plan.tick,
            from_level: plan.from_level,
            entries: plan.entries.clone(),
        },
    );
    Ok(())
}

// --------------------------------------------------- the session snapshot

/// The session snapshot's versioned domain: ONE content object
/// carrying the complete authoritative session — the spine save (with
/// the world section) plus the director state (the region states, the
/// checkpoints, and the structured-keyed capture ledger — the
/// materialization evidence) — published atomically through the B2
/// medium's nine-step append (the verified snapshot boundary) —
/// authoritative in engine-persist's shared session-anchor module
/// (the ONE wire-format interpretation).
pub const SESSION_SNAPSHOT_DOMAIN: &str = engine_persist::session_anchor::SESSION_SNAPSHOT_DOMAIN;

/// Encodes the complete session snapshot.
pub fn save_session(
    spine: &Spine<NpcWorld>,
    director: &RegionDirector,
) -> Result<Vec<u8>, SaveError> {
    let mut enc = Encoder::new();
    enc.write_str(SESSION_SNAPSHOT_DOMAIN);
    enc.write_bytes(&spine.save_bytes());
    let mut director_enc = Encoder::new();
    director.save(&mut director_enc);
    enc.write_bytes(&director_enc.into_bytes());
    Ok(enc.into_bytes())
}

/// Restores the complete session snapshot (fail-closed through both
/// codecs' gates; the blob consumed exactly). The container framing
/// is THE SHARED interpretation (engine-persist's session-anchor
/// module — the same decode the authenticated rotation boundary
/// derives from; never a second container reading).
pub fn restore_session(bytes: &[u8]) -> Result<(Spine<NpcWorld>, RegionDirector), SaveError> {
    let (spine_bytes, director_bytes) =
        engine_persist::session_anchor::read_snapshot_container(bytes)?;
    let spine = Spine::<NpcWorld>::restore(&spine_bytes, Diagnostics::new(Level::Off))?;
    let mut director_dec = Decoder::new(&director_bytes);
    let director = RegionDirector::restore(&mut director_dec)?;
    Ok((spine, director))
}

/// The located restoration of the latest chain session snapshot (the
/// recovery's input): the spine, the director, and the snapshot's
/// content and revision identities (the C1 covering track's recovery
/// seed).
type LatestSnapshot = (Spine<NpcWorld>, RegionDirector, Hash, Hash);

/// The latest session snapshot on the chain: walk from HEAD through
/// the parents, reading each revision's first content object. The
/// FIRST object recognizable as a session snapshot (the
/// domain-family tag) is the NEWEST authoritative session state: it
/// must restore — a recognizable-but-invalid newest session object
/// fails CLOSED (corruption of the newest authoritative state is
/// never walked past to an older snapshot, which would resurrect
/// stale authoritative state). The founding record carries no
/// content; the walk is bounded by the chain length.
fn latest_snapshot_at<S: MediumSource>(
    source: &S,
) -> Result<Option<LatestSnapshot>, TransitionError> {
    let mut medium = source.open_medium()?;
    let mut cursor = medium.head()?;
    while let Some(id) = cursor {
        let record = medium.read_record(&id)?;
        let Some(hash) = record.content_refs.first() else {
            cursor = record.parent;
            continue;
        };
        let bytes = medium.read_object(hash)?;
        if is_session_object(&bytes) {
            // The snapshot's content identity (for the C1 covering
            // track's recovery seed) and its carrying revision.
            let content_hash = StoredObject::new(bytes.clone())?.hash();
            return restore_session(&bytes)
                .map(|pair| Some((pair.0, pair.1, content_hash, id)))
                .map_err(|_| TransitionError::CorruptSnapshot("newest-snapshot-restore-failed"));
        }
        cursor = record.parent;
    }
    Ok(None)
}

/// Whether a content object is recognizable as a session snapshot:
/// the object's first wire field (the u64-length-prefixed domain
/// string) carries the session-snapshot domain FAMILY — any version.
/// Recognition peeks the tag only, never committing to the body: a
/// future version's snapshot is still recognizable, so this build
/// fails closed on its restore rather than silently using older
/// state. The close's idempotence walk shares this recognizer
/// (C4-R4): its supersession classification must match the recovery
/// selection class exactly — a recognizable future-version snapshot
/// intervenes exactly like a different supported one, never silently
/// passed as a non-snapshot.
pub(crate) fn is_session_object(bytes: &[u8]) -> bool {
    engine_persist::session_anchor::is_session_object(bytes)
}

/// The recovery anchor from the LEDGER's own per-identity state —
/// the snapshot's verified materialization evidence,
/// journal-independent (the accepted B3 coherence case: a confirmed
/// consequence already materialized in a verified snapshot requires
/// NO historical T1 journal). A pending capture's own amount is the
/// coverage; an applied identity's own amount — the persisted v3
/// per-identity evidence, never inferred from aggregate counters —
/// is the credits-lane coverage. The coherence check: the world's
/// credits must equal the sum of the ledger's applied amounts per
/// NPC, composed with the compacted lane summary's carried
/// aggregates (both session-origin — the snapshot's internal
/// coherence; a divergence is the fail-closed halt).
///
/// **The summarized coverage (PP-B2)**: for identities at or below
/// the lane summary's boundary, the anchor's coverage reconstructs
/// from the CHAIN's confirmed evidence under the boundary's
/// materialization attestation (the writer's compaction-time
/// eligibility verification: applied AND confirmed AND unreferenced)
/// — nothing is inferred, because the reconstruction's two inputs
/// are both retained (the boundary in the snapshot, the coverage on
/// the chain). The composition gate proves them equal to the carried
/// aggregates: a below-boundary identity the chain does NOT confirm
/// is an over-claiming summary — fail closed.
fn build_anchor(
    spine: &Spine<NpcWorld>,
    director: &RegionDirector,
    confirmed: &BTreeMap<ConsequenceId, ConfirmedEvidence>,
) -> Result<RecoveryAnchor, TransitionError> {
    let mut anchor = RecoveryAnchor::new(SourcePosition {
        head: None,
        tick: spine.clock().as_u64(),
    });
    for (id, amount) in director.ledger().pending_entries() {
        let counters = CounterValues {
            milestones: amount,
            pending_signals: 0,
            captured: amount,
            credits: 0,
        };
        let mut award = AwardState::new();
        for occurrence in 1..=amount {
            award.award_durable(occurrence);
        }
        anchor = anchor.with_identity(id, counters, award);
    }
    for (id, amount) in director.ledger().applied_entries() {
        let counters = CounterValues {
            milestones: amount,
            pending_signals: 0,
            captured: 0,
            credits: amount,
        };
        let mut award = AwardState::new();
        for occurrence in 1..=amount {
            award.award_durable(occurrence);
        }
        anchor = anchor.with_identity(id, counters, award);
    }
    // The summarized coverage and its composition gate (PP-B2).
    let carried_totals: Vec<u64> = director
        .ledger()
        .summarized()
        .map(|summary| summary.applied_totals.clone())
        .unwrap_or_else(|| vec![0u64; NPC_COUNT]);
    if let Some(summary) = director.ledger().summarized() {
        let mut synthesized_totals = vec![0u64; NPC_COUNT];
        for (id, evidence) in confirmed {
            if id.episode <= summary.boundary[id.region as usize] {
                let amount = evidence
                    .coverage
                    .iter()
                    .map(|classified| classified.span.hi.saturating_sub(classified.span.lo))
                    .sum();
                let counters = CounterValues {
                    milestones: amount,
                    pending_signals: 0,
                    captured: 0,
                    credits: amount,
                };
                let mut award = AwardState::new();
                for occurrence in 1..=amount {
                    award.award_durable(occurrence);
                }
                anchor = anchor.with_identity(*id, counters, award);
                synthesized_totals[id.npc as usize] =
                    synthesized_totals[id.npc as usize].saturating_add(amount);
            }
        }
        // The composition gate (fail-closed): the chain-reconstructed
        // coverage must compose EXACTLY to the carried aggregates —
        // an identity the chain does not confirm, or a coverage
        // disagreement, is an over-claiming summary.
        for (npc, total) in synthesized_totals.iter().enumerate() {
            if *total != carried_totals[npc] {
                return Err(TransitionError::InstallIncompatible {
                    region: npc % REGION_COUNT,
                    detail: "lane-summary-composition",
                });
            }
        }
    }
    // The coherence: the world's credits == the retained applied
    // amounts + the carried aggregates, per NPC — both
    // session-origin (the snapshot's internal coherence; a
    // divergence is the halt).
    let mut applied_amounts = vec![0u64; NPC_COUNT];
    for (id, amount) in director.ledger().applied_entries() {
        applied_amounts[id.npc as usize] += amount;
    }
    for (npc, amounts) in applied_amounts.iter().enumerate() {
        let total = amounts.saturating_add(carried_totals[npc]);
        if total != spine.world().credits()[npc] {
            return Err(TransitionError::InstallIncompatible {
                region: npc % REGION_COUNT,
                detail: "applied-amounts-conservation",
            });
        }
    }
    Ok(anchor)
}

// ------- The SK/MK checkpoint's range walk (PP-B3, D-PP-4) -------

/// The session-snapshot domain FAMILY — the checkpoint range-walk's
/// reclaimable-class recognition prefix (any version; the store is
/// domain-agnostic, the classification is this application's
/// policy).
const SESSION_SNAPSHOT_FAMILY: &str = "engine-slice/session-snapshot";

/// The range walk's classification for one walked chain record.
enum RangeKind {
    /// A session-snapshot publication — this checkpoint's own
    /// reclaimable range member.
    Publication,
    /// A confirming revision — a retained anchor (never reclaimable,
    /// D-PP-4a's rejection absolute).
    Anchor,
}

#[cfg(test)]
impl RangeKind {
    fn is_anchor(&self) -> bool {
        matches!(self, RangeKind::Anchor)
    }
}

/// One walked chain record with its classification.
struct WalkedRecord {
    record: RevisionRecord,
    kind: RangeKind,
}

/// Peeks a content object's leading domain tag (the classification
/// input; no payload parsed).
fn peek_object_domain(bytes: &[u8]) -> Option<String> {
    Decoder::new(bytes)
        .read_str(MAX_TAG_LEN, "checkpoint-walk")
        .ok()
        .map(|domain| domain.to_owned())
}

/// Walks the chain from SK's parent down to the range boundary —
/// the prior checkpoint's MK (the permanent boundary; its metadata
/// carries the campaign's founding-derived digest seed) or the
/// founding record (the root; the fresh seed) — classifying each
/// record by its content object's domain. Returns the walked records
/// in DESCENT order (newest first). An unclassifiable record (an
/// unknown content domain, or a regular record carrying no content
/// reference) refuses the checkpoint — the walk never guesses.
fn walk_checkpoint_range(
    medium: &mut dyn CanonicalMedium,
    sk_parent: Hash,
) -> Result<(Vec<WalkedRecord>, Hash, [u8; 32]), TransitionError> {
    let mut walked = Vec::new();
    let mut cursor = sk_parent;
    loop {
        let record = medium.read_record(&cursor).map_err(PipelineError::Medium)?;
        let Some(parent) = record.parent else {
            // The founding — the root boundary; the fresh
            // founding-derived seed.
            let boundary = record.id();
            let seed = compute_digest_seed(&boundary);
            return Ok((walked, boundary, seed));
        };
        let Some(first) = record.content_refs.first().copied() else {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "checkpoint-walk/unclassifiable",
            });
        };
        let bytes = medium.read_object(&first).map_err(PipelineError::Medium)?;
        let Some(domain) = peek_object_domain(&bytes) else {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "checkpoint-walk/unreadable-domain",
            });
        };
        if domain.starts_with(SESSION_SNAPSHOT_FAMILY) {
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Publication,
            });
        } else if domain == CONSEQUENCE_BATCH_DOMAIN {
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Anchor,
            });
        } else if domain == RECLAMATION_LEDGER_DOMAIN {
            // A canonical reclamation audit (the review corrective
            // R2): RETAINED EVIDENCE — the explicit+logged record of
            // a prior reclamation, never a reclaimable snapshot
            // publication. The strict decode is the fail-closed gate
            // (an invalid, missing, or incoherent ledger object
            // refuses the checkpoint — never merely skipped), and
            // the audit's retained position participates in the
            // ancestry and the skip tables exactly like a
            // confirming anchor.
            ReclamationLedger::decode(&bytes).map_err(PipelineError::Format)?;
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Anchor,
            });
        } else if domain == RECLAMATION_COMPLETE_DOMAIN {
            // The K2-C2B completion marker (P2-F4: the reader arm
            // ships BEFORE any writer — no marker-bearing history
            // can exist until the P2 execution switch): RETAINED
            // EVIDENCE — the terminal-completion record,
            // strict-decoded (an invalid or incoherent marker object
            // refuses the checkpoint — never merely skipped), its
            // retained position participating in the ancestry and
            // the skip tables exactly like the audit arm.
            ReclamationComplete::decode(&bytes).map_err(PipelineError::Format)?;
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Anchor,
            });
        } else if engine_persist::reclaim::is_reclamation_begin_family(&domain) {
            // The OC-C1 ratified begin-record reader arm: RETAINED
            // EVIDENCE (the durable execution-authorization record),
            // strict-decoded — an invalid begin object refuses the
            // checkpoint, never merely skipped. A begin-FAMILY
            // domain other than the ratified /1 is UNSUPPORTED, not
            // ignorable content. Reader recognition is NOT execution
            // authorization: encountering a begin never licenses
            // destructive reclamation, completion certification, or
            // historical attribution (the authority checks belong to
            // the later separately authorized checkpoints).
            if domain != engine_persist::reclaim::RECLAMATION_BEGIN_DOMAIN {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "checkpoint-walk/begin-unsupported-version",
                });
            }
            engine_persist::reclaim::ReclamationBegin::decode(&bytes)
                .map_err(PipelineError::Format)?;
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Anchor,
            });
        } else if domain == engine_persist::reclaim::JOURNAL_ROOT_BINDING_DOMAIN {
            // (V2-W F4) The campaign's journal-root binding —
            // PERMANENT CAMPAIGN METADATA (the bound lifecycle's
            // authority record, published at the founding by the
            // trusted bootstrap): RETAINED EVIDENCE, strict-decoded
            // (an invalid binding object refuses the checkpoint —
            // never merely skipped), its retained position
            // participating in the ancestry and the skip tables
            // exactly like the audit, marker, and begin arms. The
            // binding is never a reclaimable snapshot publication.
            engine_persist::reclaim::JournalRootBinding::decode(&bytes)
                .map_err(PipelineError::Format)?;
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Anchor,
            });
        } else if domain == engine_persist::reclaim::RECLAMATION_PLAN_DOMAIN {
            // (V2-W) The trusted reclamation PLAN — the complete
            // membership partition published BEFORE any destructive
            // work (the all-audits-first protocol's authority
            // record): RETAINED EVIDENCE, strict-decoded (an
            // invalid plan object refuses the checkpoint — never
            // merely skipped). The plan is permanent intent
            // metadata, never a reclaimable snapshot publication.
            engine_persist::reclaim::ReclaimPlan::decode(&bytes).map_err(PipelineError::Format)?;
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Anchor,
            });
        } else if domain == engine_persist::reclaim::RECLAMATION_LEDGER_V2_DOMAIN {
            // (V2-W) The v2 batch audit — one per plan-defined
            // batch, the batch's provenance-preserving evidence:
            // RETAINED EVIDENCE, strict-decoded (an invalid audit
            // object refuses the checkpoint — never merely skipped),
            // exactly like the v1 audit arm above.
            engine_persist::reclaim::ReclamationLedgerV2::decode(&bytes)
                .map_err(PipelineError::Format)?;
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Anchor,
            });
        } else if engine_persist::ancestry_witness::is_reclamation_ancestry_witness_family(&domain)
        {
            // (A1-R) The ancestry-witness family — the manifest and
            // the shards, the record-preimage evidence backing the
            // post-deletion ancestry verification: RETAINED
            // EVIDENCE, never a reclaimable snapshot publication;
            // the witness revision AND its referenced content are
            // preserved through every later checkpoint and
            // retirement selection. The recognition is LOCAL to
            // this one revision — its authorized shape, its exact
            // ratified domain and version, its strict A0 decode,
            // and its deterministic idempotency-key binding — and
            // is deliberately NOT a completeness authority: the
            // accepted A1-3 public verifier remains the
            // installed-state authority, and a manifestless shard
            // prefix is an explicitly supported recovery state (one
            // witness revision's recognition never depends on its
            // siblings being installed). No Verified outcome, no
            // new durability tier, and no destructive authority is
            // manufactured here. A family-domain version other
            // than the ratified /1 forms is UNSUPPORTED — the typed
            // incompatibility, never an ignorable unknown record
            // and never a reclaimable snapshot.
            if domain != engine_persist::ancestry_witness::RECLAMATION_WITNESS_MANIFEST_DOMAIN
                && domain != engine_persist::ancestry_witness::RECLAMATION_WITNESS_SHARD_DOMAIN
            {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "checkpoint-walk/witness-unsupported-version",
                });
            }
            // The authorized revision shape: a REGULAR revision
            // with EXACTLY ONE witness content reference (the
            // publisher's own publication shape — anything else is
            // not a witness revision).
            if !matches!(record.kind, engine_persist::record::RecordKind::Regular)
                || record.content_refs.len() != 1
            {
                return Err(TransitionError::InstallIncompatible {
                    region: 0,
                    detail: "checkpoint-walk/witness-shape-mismatch",
                });
            }
            if domain == engine_persist::ancestry_witness::RECLAMATION_WITNESS_MANIFEST_DOMAIN {
                let manifest =
                    engine_persist::ancestry_witness::AncestryWitnessManifest::decode(&bytes)
                        .map_err(PipelineError::Format)?;
                if record.idempotency_key.as_deref()
                    != Some(
                        &engine_persist::ancestry_witness::reclamation_witness_manifest_idempotency_key(
                            &manifest.mk,
                        )[..],
                    )
                {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "checkpoint-walk/witness-key-mismatch",
                    });
                }
            } else {
                let shard = engine_persist::ancestry_witness::AncestryWitnessShard::decode(&bytes)
                    .map_err(PipelineError::Format)?;
                if record.idempotency_key.as_deref()
                    != Some(
                        &engine_persist::ancestry_witness::reclamation_witness_shard_idempotency_key(
                            &shard.mk,
                            shard.shard_index,
                        )[..],
                    )
                {
                    return Err(TransitionError::InstallIncompatible {
                        region: 0,
                        detail: "checkpoint-walk/witness-key-mismatch",
                    });
                }
            }
            walked.push(WalkedRecord {
                record,
                kind: RangeKind::Anchor,
            });
        } else if domain == CHECKPOINT_METADATA_DOMAIN {
            // The prior checkpoint's MK — the permanent boundary.
            // Its metadata carries the campaign's founding-derived
            // seed (the same seed every checkpoint of the campaign
            // uses — no re-walk below the boundary required).
            let metadata = CheckpointMetadata::decode(&bytes).map_err(PipelineError::Format)?;
            let boundary = record.id();
            return Ok((walked, boundary, metadata.seed));
        } else {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "checkpoint-walk/unclassifiable",
            });
        }
        cursor = parent;
    }
}

/// Computes the checkpoint's skip table: the non-circular
/// self-retirement entry, plus one entry for every RETAINED record
/// standing on a reclaimable parent — every confirming anchor whose
/// parent is a range member — each jumping to its NEAREST RETAINED
/// ancestor (the first non-publication record below the reclaimed
/// run; the walk's boundary when the run ends there). The
/// self-retirement entry targets SK's nearest retained ancestor:
/// sk_parent itself while sk_parent is retained (an anchor or a
/// prior boundary), or the bottom of SK's reclaim run when sk_parent
/// is itself a range publication — MK's parent SK is reclaimed
/// together with its range, so MK jumps past the whole run. A skip
/// standing on SK itself is never emitted: dead by construction
/// (nothing retained references a retired SK but MK, whose traversal
/// IS the self entry).
fn compute_checkpoint_skips(sk_parent: Hash, walked: &[WalkedRecord]) -> Vec<SkipEntry> {
    let position: HashMap<Hash, usize> = walked
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.record.id(), index))
        .collect();
    let is_publication = |hash: &Hash| {
        position
            .get(hash)
            .is_some_and(|&index| matches!(walked[index].kind, RangeKind::Publication))
    };

    let self_target = if is_publication(&sk_parent) {
        nearest_retained_ancestor(sk_parent, walked, &position)
    } else {
        sk_parent
    };
    let mut skips = vec![SkipEntry::self_skip(self_target)];
    // Every confirming anchor standing on a publication parent.
    for entry in walked {
        if matches!(entry.kind, RangeKind::Anchor)
            && let Some(parent) = entry.record.parent
            && is_publication(&parent)
        {
            let target = nearest_retained_ancestor(parent, walked, &position);
            skips.push(SkipEntry::new(entry.record.id(), target));
        }
    }
    skips
}

/// The nearest retained ancestor below a reclaimed parent: descend
/// through the contiguous publication run; the first
/// non-publication record (an anchor, or the walk's boundary below
/// the last publication) is the target.
fn nearest_retained_ancestor(
    mut cursor: Hash,
    walked: &[WalkedRecord],
    position: &HashMap<Hash, usize>,
) -> Hash {
    while let Some(&index) = position.get(&cursor)
        && matches!(walked[index].kind, RangeKind::Publication)
        && let Some(parent) = walked[index].record.parent
    {
        cursor = parent;
    }
    cursor
}

/// The reclamation selection's target: the checkpoint's MK, its SK,
/// its decoded metadata, and the installed audit when one exists
/// (the review corrective R1 — the selection carries the discovered
/// ledger so the pre-scan never re-derives it).
type ReclaimTarget = (
    Hash,
    Hash,
    CheckpointMetadata,
    Option<(Hash, ReclamationLedger)>,
);

/// Finds the installed reclamation audit for a checkpoint (the
/// selection's re-derivation): the bounded descent from the head
/// down TO the target MK — the audit always sits above its
/// checkpoint. THE COMPACTED MODE (the review corrective R1): the
/// per-candidate discovery now descends past candidates whose
/// audits sit below later-reclaimed ranges — an absent parent is a
/// gap, legitimate only under the most recent on-chain MK's skip
/// table (the record's first content object carrying the
/// checkpoint-metadata domain, the established gap-aware descent);
/// anything else is hard corruption, fail closed — never a
/// legitimate gap reinterpreted as corruption. A
/// reclamation-ledger/1 first content object decodes and binds by
/// its mk.
fn find_installed_reclamation(
    medium: &mut dyn CanonicalMedium,
    target_mk: &Hash,
) -> Result<Option<(Hash, ReclamationLedger)>, TransitionError> {
    let Some(mut current) = medium.head().map_err(PipelineError::Medium)? else {
        return Ok(None);
    };
    let mut skip_table: Option<(Hash, CheckpointMetadata)> = None;
    loop {
        if current == *target_mk {
            // The target checkpoint itself: its object is the
            // checkpoint metadata, never an audit — the bounded
            // descent ends here.
            return Ok(None);
        }
        let record = medium
            .read_record(&current)
            .map_err(PipelineError::Medium)?;
        let parent = record.parent;
        if let Some(object) = record.content_refs.first() {
            let bytes = medium.read_object(object).map_err(PipelineError::Medium)?;
            if let Some(domain) = peek_object_domain(&bytes).as_deref() {
                if domain == RECLAMATION_LEDGER_DOMAIN {
                    let ledger =
                        ReclamationLedger::decode(&bytes).map_err(PipelineError::Format)?;
                    if &ledger.mk == target_mk {
                        return Ok(Some((current, ledger)));
                    }
                } else if domain == CHECKPOINT_METADATA_DOMAIN {
                    let metadata =
                        CheckpointMetadata::decode(&bytes).map_err(PipelineError::Format)?;
                    skip_table = Some((current, metadata));
                }
            }
        }
        let Some(parent) = parent else {
            return Ok(None);
        };
        match medium.read_record(&parent) {
            Ok(_) => current = parent,
            Err(MediumError::MissingObject { .. }) => {
                let Some((owner, table)) = skip_table.as_ref() else {
                    return Err(
                        PipelineError::Medium(MediumError::MissingObject { hash: parent }).into(),
                    );
                };
                let jump = if current == *owner {
                    table
                        .self_skip()
                        .map(|entry| entry.nearest_retained_ancestor)
                } else {
                    table
                        .skip_for(&current)
                        .map(|entry| entry.nearest_retained_ancestor)
                };
                let Some(jump) = jump else {
                    return Err(
                        PipelineError::Medium(MediumError::MissingObject { hash: parent }).into(),
                    );
                };
                current = jump;
            }
            Err(other) => return Err(PipelineError::Medium(other).into()),
        }
    }
}

/// The stale journal generations removable alongside the range
/// (PP-B3's Phase B, the review correctives F1 + R3): only after
/// the rotation's verified active journal stands — no pointer means
/// no rotation, and the campaign root journal IS the active one
/// (never touched). THE OWNERSHIP RECORD (R3): the eligible stale
/// generations are the CAMPAIGN'S OWN recorded lineage — the
/// generations manifest the rotation maintains beside the root —
/// NEVER a directory-wide filename pattern (in a shared directory a
/// foreign campaign's files match the journal-*.t1 predicate; only
/// the campaign's own rotation lineage is deletion authority). A
/// missing record (a pre-rotation campaign, or a generation
/// unrecorded by a crash between the flip and the record's write)
/// means the unlisted generations are NOT eligible — ownership
/// conservatively unproven, the files retained.
fn reclaimable_journal_files(root: &Path) -> Result<Vec<String>, TransitionError> {
    let active = active_journal_path(root).map_err(PipelineError::Journal)?;
    if active == root {
        return Ok(Vec::new());
    }
    // The active journal must scan green before any stale generation
    // is removable (the survival set stands).
    let active_name = active
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    T1Journal::scan_file(active).map_err(PipelineError::Journal)?;
    let names = read_journal_generations(root).map_err(PipelineError::Journal)?;
    let mut stale = Vec::new();
    for name in names {
        if active_name.as_deref() == Some(name.as_str()) {
            continue;
        }
        stale.push(name);
    }
    stale.sort();
    Ok(stale)
}

#[cfg(test)]
mod oc_c1_reader_tests {
    use super::*;
    use engine_persist::local::LocalFileMedium;
    use engine_persist::medium::CanonicalMedium;
    use engine_persist::reclaim::{
        RECLAMATION_BEGIN_DOMAIN, ReclamationBegin, test_only_install_begin_record,
    };
    use engine_persist::record::{AuthorityMode, RevisionRecord};
    use engine_persist::{Hash, StoredObject};

    fn temp_store(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "s7-occ1-reader-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn the_begin_record_reads_as_retained_evidence() {
        // (OC-C1-C) A valid begin record installed through the
        // compile-time-isolated test-only facility is classified as
        // RETAINED EVIDENCE (the anchor kind) — it participates in
        // the ancestry without entering the retiring-publication
        // set, and the walk completes to the founding. Reader
        // recognition grants NO execution authority (the authority
        // checks belong to the later checkpoints).
        let root = temp_store("retained");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "occ1r", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let begin = ReclamationBegin {
            mk: founding.id(),
            source: founding.id(),
            range_digest: [0x03; 32],
            seed: [0x04; 32],
        };
        let begin_id = test_only_install_begin_record(&mut medium, &begin, 9).unwrap();
        // The walk starting AT the begin record: it is walked as an
        // Anchor, then the descent reaches the founding.
        let (walked, boundary, _seed) = walk_checkpoint_range(&mut medium, begin_id).unwrap();
        assert_eq!(boundary, founding.id());
        let begin_entry = walked
            .iter()
            .find(|entry| entry.record.id() == begin_id)
            .expect("the begin record is walked");
        assert!(
            matches!(begin_entry.kind, RangeKind::Anchor),
            "the begin is retained evidence, never a retiring publication"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_unsupported_begin_family_version_refuses_the_walk() {
        // (OC-C1-C) A begin-FAMILY domain other than the ratified /1
        // is UNSUPPORTED, not ignorable content: the walk refuses
        // typed. The fixture hand-writes the object and record files
        // (the local store's content-addressed layout) — the generic
        // append's family gate makes on-chain publication impossible,
        // so the only construction is the direct file write.
        let root = temp_store("unsupported");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "occ1u", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let unsupported = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("engine-persist/reclamation-begin/2");
            enc.write_u32(1);
            enc.write_bytes(&[0x01; 32]);
            enc.write_bytes(&[0x02; 32]);
            enc.write_bytes(&[0x03; 32]);
            enc.write_bytes(&[0x04; 32]);
            enc.into_bytes()
        })
        .unwrap();
        let record =
            RevisionRecord::regular(head, epoch, 9).with_content_refs(vec![unsupported.hash()]);
        // The direct file write (the test-only construction; no
        // production publication path exists for the family).
        std::fs::write(
            medium.object_path_pub(&unsupported.hash()),
            unsupported.bytes(),
        )
        .unwrap();
        std::fs::write(
            medium.object_path_pub(&record.id()),
            record.canonical_bytes(),
        )
        .unwrap();
        let detail = match walk_checkpoint_range(&mut medium, record.id()) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the unsupported version refuses, got {other:?}"),
            Ok(_) => panic!("the unsupported version never walks"),
        };
        assert_eq!(detail, "checkpoint-walk/begin-unsupported-version");
        let _ = RECLAMATION_BEGIN_DOMAIN;
        let _ = Hash::ZERO;
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_malformed_begin_refuses_the_walk() {
        // (OC-C1-C) A begin-family record whose object fails the
        // strict decode (here: a truncated canonical form) refuses
        // the walk typed — never merely skipped.
        let root = temp_store("malformed");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "occ1m", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let begin = ReclamationBegin {
            mk: founding.id(),
            source: founding.id(),
            range_digest: [0x03; 32],
            seed: [0x04; 32],
        };
        let truncated = &begin.canonical_bytes()[..100];
        let malformed_object = StoredObject::new(truncated.to_vec()).unwrap();
        let record = RevisionRecord::regular(head, epoch, 9)
            .with_content_refs(vec![malformed_object.hash()]);
        std::fs::write(
            medium.object_path_pub(&malformed_object.hash()),
            malformed_object.bytes(),
        )
        .unwrap();
        std::fs::write(
            medium.object_path_pub(&record.id()),
            record.canonical_bytes(),
        )
        .unwrap();
        assert!(walk_checkpoint_range(&mut medium, record.id()).is_err());
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
}

// -------------------------------------------------------------
// (V2X-CP3-A1-R) THE WITNESS-FAMILY RECOGNITION REGRESSIONS —
// the manifest and shard revisions read as RETAINED EVIDENCE,
// excluded from the retirement membership and digest, skipped
// correctly over retiring snapshots, recognized manifestless,
// and refusing every unsupported or malformed form. The
// recognition grants NO completeness authority: the A1-3 public
// verifier remains the installed-state authority.
// -------------------------------------------------------------

#[cfg(test)]
mod a1r_reader_tests {
    use super::*;
    use engine_persist::ancestry_witness::{
        WitnessEntry, construct_witness, publish_ancestry_witness,
        reclamation_witness_manifest_idempotency_key, reclamation_witness_shard_idempotency_key,
    };
    use engine_persist::local::LocalFileMedium;
    use engine_persist::medium::CanonicalMedium;
    use engine_persist::record::{AuthorityMode, RevisionRecord};
    use engine_persist::{Hash, StoredObject};

    fn temp_store(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "s7-a1r-reader-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn temp_journal(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "s7-a1r-journal-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// An ordinary legacy session-snapshot object — the walk's
    /// Publication classification and the generic append's allowed
    /// /1 form.
    fn snapshot_object(payload: &[u8]) -> StoredObject {
        StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("engine-slice/session-snapshot/1");
            enc.write_bytes(payload);
            enc.into_bytes()
        })
        .unwrap()
    }

    /// Appends one ordinary session-snapshot publication (the
    /// reclaimable range member).
    fn append_snapshot(medium: &mut dyn CanonicalMedium, payload: &[u8], tick: u64) -> Hash {
        let object = snapshot_object(payload);
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record =
            RevisionRecord::regular(head, epoch, tick).with_content_refs(vec![object.hash()]);
        medium
            .append(&record, std::slice::from_ref(&object), Some(head))
            .unwrap();
        record.id()
    }

    /// The direct file-write record installation (the established
    /// test-only construction for gated families — the generic
    /// append's family gates make on-chain publication impossible,
    /// so the record and object files are written directly into the
    /// local store's content-addressed layout; the WALK follows the
    /// parent chain from a given start, so the medium's HEAD
    /// pointer is irrelevant to the walked range).
    fn write_record_with_object(
        medium: &mut LocalFileMedium,
        parent: Hash,
        key: Option<Vec<u8>>,
        object: &StoredObject,
        tick: u64,
    ) -> Hash {
        let parent_epoch = medium.read_record(&parent).unwrap().epoch;
        let record = RevisionRecord::regular(parent, parent_epoch, tick)
            .with_content_refs(vec![object.hash()]);
        let record = match key {
            Some(key) => record.with_idempotency_key(key),
            None => record,
        };
        std::fs::write(medium.object_path_pub(&object.hash()), object.bytes()).unwrap();
        std::fs::write(
            medium.object_path_pub(&record.id()),
            record.canonical_bytes(),
        )
        .unwrap();
        record.id()
    }

    /// The epoch of the current top record (for the direct-write
    /// parent chaining).
    fn top_epoch(medium: &mut LocalFileMedium, top: Hash) -> u64 {
        medium.read_record(&top).unwrap().epoch
    }

    /// The genuine CP2 campaign plus the accepted test-gated A1-2
    /// witness publication: the founding, the bound journal, p1,
    /// two retiring members, SK, the checkpoint metadata and MK,
    /// the plan, every audit, the V2 begin, and the publisher's
    /// shard + manifest above it. Returns (mk, manifest_record,
    /// shard_record) — the campaign's checkpoint boundary and the
    /// two witness revisions.
    #[allow(clippy::type_complexity)]
    fn genuine_witness_publication(
        medium: &mut dyn CanonicalMedium,
        label: &str,
        journal_root: &std::path::Path,
    ) -> (Hash, Hash, Hash) {
        let shared = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"a1r-shared-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let founding_id = founding.id();
        let location =
            engine_persist::reclaim::JournalRootBinding::normalize_location(journal_root);
        let root_id = engine_persist::reclaim::journal_root_id(&founding_id, &location);
        drop(
            engine_persist::reclaim::create_campaign_bound_journal(
                medium,
                journal_root,
                &founding_id,
            )
            .unwrap(),
        );
        let (_binding, outcome) = engine_persist::reclaim::publish_journal_root_binding(
            medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            engine_persist::reclaim::JournalRootBindingOutcome::Accepted { .. }
        ));
        let step = |medium: &mut dyn CanonicalMedium, tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record =
                RevisionRecord::regular(head, epoch, tick).with_content_refs(vec![shared.hash()]);
            medium
                .append(&record, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            record.id()
        };
        let p1_id = step(medium, 2);
        let mut member_ids = Vec::new();
        for index in 0..2u32 {
            let object = StoredObject::new({
                let mut enc = engine_core::Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(format!("a1r-{label}-member-{index}").as_bytes());
                enc.into_bytes()
            })
            .unwrap();
            let key = format!("a1r-{label}-member-key-{index}").into_bytes();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, 3)
                .with_idempotency_key(key)
                .with_content_refs(vec![object.hash()]);
            medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap();
            member_ids.push(record.id());
        }
        let sk_id = step(medium, 4);
        let seed = engine_persist::checkpoint::compute_digest_seed(&founding_id);
        let mut digest = seed;
        let mut index = Vec::with_capacity(3);
        for id in &member_ids {
            let record = medium.read_record(id).unwrap();
            digest = engine_persist::checkpoint::chain_range_digest_step(
                &digest,
                &record.canonical_bytes(),
            );
            index.push(engine_persist::checkpoint::ReclaimedIndexEntry::new(
                record.idempotency_key.clone(),
                *id,
                record.epoch,
            ));
        }
        let sk_record = medium.read_record(&sk_id).unwrap();
        index.push(engine_persist::checkpoint::ReclaimedIndexEntry::new(
            sk_record.idempotency_key.clone(),
            sk_id,
            sk_record.epoch,
        ));
        let metadata = engine_persist::checkpoint::CheckpointMetadata::new(
            sk_id,
            member_ids[0],
            vec![engine_persist::checkpoint::SkipEntry::new(
                Hash::ZERO,
                p1_id,
            )],
            index,
            digest,
            seed,
        );
        let metadata_object = StoredObject::new(metadata.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk =
            RevisionRecord::regular(head, epoch, 5).with_content_refs(vec![metadata_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&metadata_object), Some(head))
            .unwrap();
        let mk_id = mk.id();
        engine_persist::reclaim::publish_reclamation_plan(medium, &mk_id, 10_000, journal_root, 6)
            .unwrap();
        let (_plan_id, plan) = engine_persist::reclaim::find_installed_plan(medium, &mk_id)
            .unwrap()
            .unwrap();
        for batch in &plan.batches {
            engine_persist::reclaim::publish_reclamation_ledger_v2(medium, &mk_id, batch.index, 7)
                .unwrap();
        }
        medium.begin_reclamation_v2(&mk_id, 8).unwrap();
        let publication = publish_ancestry_witness(medium, &mk_id, 9).unwrap();
        let shard_record = match publication.shard_outcomes[0] {
            engine_persist::ancestry_witness::WitnessPublicationOutcome::Accepted {
                record,
                ..
            } => record,
            other => panic!("the genuine shard publishes: {other:?}"),
        };
        (mk_id, publication.manifest_record, shard_record)
    }

    /// (R1) The genuine ratified publication through the accepted
    /// test-gated A1-2 mechanisms: both the manifest and the shard
    /// revisions are recognized as RETAINED EVIDENCE (the anchor
    /// kind), never as reclaimable publications, and the walk
    /// terminates at the campaign's own MK checkpoint boundary.
    #[test]
    fn the_witness_manifest_and_shard_read_as_retained_evidence() {
        let root = temp_store("retained");
        let journal = temp_journal("retained");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let (mk, manifest_record, shard_record) =
            genuine_witness_publication(&mut medium, "retained", &journal);
        let (walked, boundary, _seed) =
            walk_checkpoint_range(&mut medium, manifest_record).unwrap();
        assert_eq!(boundary, mk, "the walk terminates at the campaign MK");
        for record_id in [manifest_record, shard_record] {
            let entry = walked
                .iter()
                .find(|entry| entry.record.id() == record_id)
                .expect("the witness revision is walked");
            assert!(
                matches!(entry.kind, RangeKind::Anchor),
                "the witness revision {record_id} is retained evidence, never a retiring publication"
            );
        }
        assert!(
            !walked
                .iter()
                .any(|entry| matches!(entry.kind, RangeKind::Publication)),
            "the genuine campaign's witness range contains no reclaimable snapshot"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (R7) The SQLite twin: the genuine publication and the
    /// recognition on the transactional provider.
    #[test]
    fn the_witness_sqlite_twin_reads_as_retained_evidence() {
        let path = std::env::temp_dir().join(format!(
            "s7-a1r-sqlite-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let journal = temp_journal("sqlite");
        let mut medium = engine_persist::sqlite::SqliteMedium::open(&path).unwrap();
        let (mk, manifest_record, shard_record) =
            genuine_witness_publication(&mut medium, "sqlite", &journal);
        let (walked, boundary, _seed) =
            walk_checkpoint_range(&mut medium, manifest_record).unwrap();
        assert_eq!(boundary, mk);
        for record_id in [manifest_record, shard_record] {
            let entry = walked
                .iter()
                .find(|entry| entry.record.id() == record_id)
                .expect("the witness revision is walked");
            assert!(matches!(entry.kind, RangeKind::Anchor));
        }
        drop(medium);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (R2) The checkpoint range separation: ordinary reclaimable
    /// snapshot publications interleaved with retained witness
    /// revisions — the retirement index (the caller's own
    /// construction over the walked Publication kinds) contains
    /// exactly the eligible snapshots and excludes every witness
    /// revision, and the range digest folds over exactly the
    /// snapshot-only membership.
    #[test]
    fn the_witness_exclusions_from_the_retirement_index_and_digest() {
        let root = temp_store("separation");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"a1r-separation-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "separation", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let founding_id = founding.id();
        let snap1 = append_snapshot(&mut medium, b"a1r-snap-1", 10);
        let snap1_bytes = medium.read_record(&snap1).unwrap().canonical_bytes();
        // The interleaved chain above the appended snapshot: the
        // witness shard, an ordinary snapshot record, and the
        // witness manifest — the witness records direct-written
        // (the generic append's family gates refuse the witness
        // objects; the walk follows the parent chain from the
        // given start, so the HEAD pointer is irrelevant).
        let witness_mk = engine_persist::hash::hash_object(b"a1r-separation-witness-mk");
        let witness_plan = engine_persist::hash::hash_object(b"a1r-separation-witness-plan");
        let witness_begin = engine_persist::hash::hash_object(b"a1r-separation-witness-begin");
        let witness_metadata =
            engine_persist::hash::hash_object(b"a1r-separation-witness-metadata");
        let (manifest, shards) = construct_witness(
            &witness_mk,
            &witness_plan,
            &witness_begin,
            &witness_metadata,
            vec![WitnessEntry {
                member: snap1,
                preimage: snap1_bytes.clone(),
            }],
        )
        .unwrap();
        let shard_object = StoredObject::new(shards[0].canonical_bytes()).unwrap();
        let manifest_object = StoredObject::new(manifest.canonical_bytes()).unwrap();
        let shard_record = write_record_with_object(
            &mut medium,
            snap1,
            Some(reclamation_witness_shard_idempotency_key(&witness_mk, 0)),
            &shard_object,
            11,
        );
        let snap2 = write_record_with_object(
            &mut medium,
            shard_record,
            None,
            &snapshot_object(b"a1r-snap-2"),
            12,
        );
        let snap2_bytes = medium.read_record(&snap2).unwrap().canonical_bytes();
        let manifest_record = write_record_with_object(
            &mut medium,
            snap2,
            Some(reclamation_witness_manifest_idempotency_key(&witness_mk)),
            &manifest_object,
            13,
        );
        let (walked, boundary, seed) = walk_checkpoint_range(&mut medium, manifest_record).unwrap();
        assert_eq!(boundary, founding_id);
        // The classifications: the witness revisions are anchors
        // and the ordinary snapshots are publications — interleaved
        // in the walked range.
        let is_anchor = |id: Hash| {
            walked
                .iter()
                .find(|entry| entry.record.id() == id)
                .expect("the record is walked")
                .kind
                .is_anchor()
        };
        assert!(is_anchor(manifest_record));
        assert!(is_anchor(shard_record));
        assert!(!is_anchor(snap2));
        assert!(!is_anchor(snap1));
        // The caller's own retirement-index construction over the
        // walked Publication kinds: exactly the eligible snapshots,
        // every witness revision excluded.
        let mut range = Vec::new();
        let mut index = Vec::new();
        let mut chain_bytes = Vec::new();
        for entry in &walked {
            if matches!(entry.kind, RangeKind::Publication) {
                range.push(entry.record.id());
                index.push(engine_persist::checkpoint::ReclaimedIndexEntry::new(
                    entry.record.idempotency_key.clone(),
                    entry.record.id(),
                    entry.record.epoch,
                ));
                chain_bytes.push(entry.record.canonical_bytes());
            }
        }
        assert_eq!(range, vec![snap2, snap1], "the descent order");
        assert!(!index.iter().any(|entry| entry.hash == manifest_record));
        assert!(!index.iter().any(|entry| entry.hash == shard_record));
        // The digest: the snapshot-only membership, oldest first —
        // equal to the independently computed fold over exactly
        // [snap1, snap2] (the witness bytes never enter it).
        chain_bytes.reverse();
        let range_digest = chain_range_digest(&seed, &chain_bytes);
        let expected = chain_range_digest(&seed, &[snap1_bytes, snap2_bytes.clone()]);
        assert_eq!(range_digest, expected);
        let _ = manifest_record;
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (R3) The retained-ancestry and skip-table integrity: a
    /// retained witness revision standing immediately above a
    /// reclaimable snapshot receives the correct
    /// nearest-retained-ancestor skip when its parent would be
    /// retired, and a witness revision standing on another anchor
    /// receives none — checked through the walk and the skip
    /// computation, without executing any destructive retirement.
    #[test]
    fn the_witness_above_a_retiring_snapshot_receives_the_skip() {
        let root = temp_store("skip");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"a1r-skip-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "skip", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let founding_id = founding.id();
        let snap1 = append_snapshot(&mut medium, b"a1r-skip-snap-1", 10);
        let snap1_bytes = medium.read_record(&snap1).unwrap().canonical_bytes();
        let snap2 = append_snapshot(&mut medium, b"a1r-skip-snap-2", 11);
        let witness_mk = engine_persist::hash::hash_object(b"a1r-skip-witness-mk");
        let witness_plan = engine_persist::hash::hash_object(b"a1r-skip-witness-plan");
        let witness_begin = engine_persist::hash::hash_object(b"a1r-skip-witness-begin");
        let witness_metadata = engine_persist::hash::hash_object(b"a1r-skip-witness-metadata");
        let (_manifest, shards) = construct_witness(
            &witness_mk,
            &witness_plan,
            &witness_begin,
            &witness_metadata,
            vec![WitnessEntry {
                member: snap1,
                preimage: snap1_bytes,
            }],
        )
        .unwrap();
        let shard_object = StoredObject::new(shards[0].canonical_bytes()).unwrap();
        // The witness shard directly above the reclaimable snap2:
        // its parent WOULD be retired, so it must receive the skip
        // to its nearest retained ancestor (the founding, past the
        // whole reclaim run).
        let shard_record = write_record_with_object(
            &mut medium,
            snap2,
            Some(reclamation_witness_shard_idempotency_key(&witness_mk, 0)),
            &shard_object,
            12,
        );
        let (walked, boundary, _seed) = walk_checkpoint_range(&mut medium, shard_record).unwrap();
        assert_eq!(boundary, founding_id);
        let skips = compute_checkpoint_skips(shard_record, &walked);
        // The standing skip for the witness shard on the retiring
        // snapshot: the target is the founding (the first
        // non-publication record below the contiguous reclaim run
        // snap2 → snap1).
        assert!(skips.contains(&SkipEntry::new(shard_record, founding_id)));
        // The self-entry targets the walk's start (an anchor, not a
        // publication — SK's own parent is retained).
        assert!(skips.contains(&SkipEntry::self_skip(shard_record)));
        // A witness revision standing on another ANCHOR receives no
        // standing skip: the begin anchor (appended through the
        // accepted test-only facility) above the snapshot, with the
        // shard above the begin.
        let begin = engine_persist::reclaim::ReclamationBegin {
            mk: founding_id,
            source: founding_id,
            range_digest: [0x03; 32],
            seed: [0x04; 32],
        };
        let begin_id =
            engine_persist::reclaim::test_only_install_begin_record(&mut medium, &begin, 13)
                .unwrap();
        let shard_on_anchor = write_record_with_object(
            &mut medium,
            begin_id,
            Some(reclamation_witness_shard_idempotency_key(&witness_mk, 0)),
            &shard_object,
            14,
        );
        let (walked, boundary, _seed) =
            walk_checkpoint_range(&mut medium, shard_on_anchor).unwrap();
        assert_eq!(boundary, founding_id);
        let skips = compute_checkpoint_skips(shard_on_anchor, &walked);
        // The begin anchor standing on the retiring snapshot gets
        // ITS skip; the witness shard standing on the begin gets
        // NO standing skip (its parent is retained — the self
        // entry's retained field is the zero marker, never the
        // shard's identity).
        assert!(skips.contains(&SkipEntry::new(begin_id, founding_id)));
        assert!(
            !skips.iter().any(|entry| entry.retained == shard_on_anchor),
            "a witness standing on a retained anchor receives no standing skip"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (R4) The manifestless prefix: a valid installed shard without
    /// any manifest is still classified as retained evidence — the
    /// recognition requires no manifest and never claims
    /// installation completeness.
    #[test]
    fn the_manifestless_shard_prefix_reads_as_retained_evidence() {
        let root = temp_store("manifestless");
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"a1r-manifestless-payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "manifestless", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let founding_id = founding.id();
        let snap1 = append_snapshot(&mut medium, b"a1r-manifestless-snap", 10);
        let snap1_bytes = medium.read_record(&snap1).unwrap().canonical_bytes();
        let witness_mk = engine_persist::hash::hash_object(b"a1r-manifestless-witness-mk");
        let witness_plan = engine_persist::hash::hash_object(b"a1r-manifestless-witness-plan");
        let witness_begin = engine_persist::hash::hash_object(b"a1r-manifestless-witness-begin");
        let witness_metadata =
            engine_persist::hash::hash_object(b"a1r-manifestless-witness-metadata");
        let (_manifest, shards) = construct_witness(
            &witness_mk,
            &witness_plan,
            &witness_begin,
            &witness_metadata,
            vec![WitnessEntry {
                member: snap1,
                preimage: snap1_bytes,
            }],
        )
        .unwrap();
        let shard_object = StoredObject::new(shards[0].canonical_bytes()).unwrap();
        // The manifestless shard prefix: no manifest record
        // anywhere on the chain.
        let shard_record = write_record_with_object(
            &mut medium,
            snap1,
            Some(reclamation_witness_shard_idempotency_key(&witness_mk, 0)),
            &shard_object,
            11,
        );
        let (walked, boundary, _seed) = walk_checkpoint_range(&mut medium, shard_record).unwrap();
        assert_eq!(boundary, founding_id);
        let entry = walked
            .iter()
            .find(|entry| entry.record.id() == shard_record)
            .expect("the manifestless shard is walked");
        assert!(
            matches!(entry.kind, RangeKind::Anchor),
            "the manifestless shard prefix is retained evidence without any manifest"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (R5) The unsupported versions and malformed evidence: every
    /// invalid witness-family state refuses the walk — never a
    /// reclaimable publication, never an ignorable unknown record.
    #[test]
    fn the_witness_family_refuses_unsupported_versions_and_malformed_evidence() {
        // The shared arm base: the founding plus one ordinary
        // snapshot (the walk's start parent).
        let arm_base = |label: &str| -> (std::path::PathBuf, LocalFileMedium, Hash, Hash, Vec<u8>) {
            let root = temp_store(label);
            let mut medium = LocalFileMedium::open(&root).unwrap();
            let plain = StoredObject::new({
                let mut enc = engine_core::Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"a1r-arm-payload");
                enc.into_bytes()
            })
            .unwrap();
            let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
                .with_content_refs(vec![plain.hash()]);
            medium
                .append(&founding, std::slice::from_ref(&plain), None)
                .unwrap();
            let snap1 = append_snapshot(&mut medium, b"a1r-arm-snap", 10);
            let snap1_bytes = medium.read_record(&snap1).unwrap().canonical_bytes();
            (root, medium, founding.id(), snap1, snap1_bytes)
        };
        // The shared valid witness objects over the arm's snapshot
        // member.
        let arm_witness = |snap1: Hash, snap1_bytes: &[u8]| -> (Hash, StoredObject, StoredObject) {
            let witness_mk = engine_persist::hash::hash_object(b"a1r-arm-witness-mk");
            let witness_plan = engine_persist::hash::hash_object(b"a1r-arm-witness-plan");
            let witness_begin = engine_persist::hash::hash_object(b"a1r-arm-witness-begin");
            let witness_metadata = engine_persist::hash::hash_object(b"a1r-arm-witness-metadata");
            let (manifest, shards) = construct_witness(
                &witness_mk,
                &witness_plan,
                &witness_begin,
                &witness_metadata,
                vec![WitnessEntry {
                    member: snap1,
                    preimage: snap1_bytes.to_vec(),
                }],
            )
            .unwrap();
            (
                witness_mk,
                StoredObject::new(manifest.canonical_bytes()).unwrap(),
                StoredObject::new(shards[0].canonical_bytes()).unwrap(),
            )
        };
        // (a) THE UNSUPPORTED MANIFEST VERSION: a family-prefixed
        // domain other than the ratified /1 refuses typed.
        let (root, mut medium, _founding, snap1, _snap1_bytes) = arm_base("unsupported-manifest");
        let unsupported = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("engine-persist/reclamation-ancestry-witness/2");
            enc.write_u32(1);
            enc.write_bytes(&[0x01; 32]);
            enc.into_bytes()
        })
        .unwrap();
        let record = write_record_with_object(&mut medium, snap1, None, &unsupported, 20);
        let detail = match walk_checkpoint_range(&mut medium, record) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the unsupported manifest version refuses, got {other:?}"),
            Ok(_) => panic!("the unsupported manifest version never walks"),
        };
        assert_eq!(detail, "checkpoint-walk/witness-unsupported-version");
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // (b) THE UNSUPPORTED SHARD VERSION.
        let (root, mut medium, _founding, snap1, _) = arm_base("unsupported-shard");
        let unsupported = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("engine-persist/reclamation-ancestry-witness-shard/2");
            enc.write_u32(1);
            enc.write_bytes(&[0x01; 32]);
            enc.into_bytes()
        })
        .unwrap();
        let record = write_record_with_object(&mut medium, snap1, None, &unsupported, 20);
        let detail = match walk_checkpoint_range(&mut medium, record) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the unsupported shard version refuses, got {other:?}"),
            Ok(_) => panic!("the unsupported shard version never walks"),
        };
        assert_eq!(detail, "checkpoint-walk/witness-unsupported-version");
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // (c) THE TRUNCATED MANIFEST: the strict decode refuses.
        let (root, mut medium, _founding, snap1, snap1_bytes) = arm_base("truncated-manifest");
        let (_mk, manifest_object, _shard_object) = arm_witness(snap1, &snap1_bytes);
        let truncated = StoredObject::new(manifest_object.bytes()[..100].to_vec()).unwrap();
        let record = write_record_with_object(&mut medium, snap1, None, &truncated, 20);
        assert!(walk_checkpoint_range(&mut medium, record).is_err());
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // (d) THE MALFORMED SHARD: a shard-domain payload that fails
        // the strict decode.
        let (root, mut medium, _founding, snap1, _) = arm_base("malformed-shard");
        let malformed = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("engine-persist/reclamation-ancestry-witness-shard/1");
            enc.write_u32(9);
            enc.into_bytes()
        })
        .unwrap();
        let record = write_record_with_object(&mut medium, snap1, None, &malformed, 20);
        assert!(walk_checkpoint_range(&mut medium, record).is_err());
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // (e) THE WRONG MANIFEST KEY: a valid manifest object under
        // a record whose idempotency key is not the deterministic
        // MK-bound form.
        let (root, mut medium, _founding, snap1, snap1_bytes) = arm_base("wrong-manifest-key");
        let (_mk, manifest_object, _shard_object) = arm_witness(snap1, &snap1_bytes);
        let record = write_record_with_object(
            &mut medium,
            snap1,
            Some(b"a1r-wrong-manifest-key".to_vec()),
            &manifest_object,
            20,
        );
        let detail = match walk_checkpoint_range(&mut medium, record) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the wrong manifest key refuses, got {other:?}"),
            Ok(_) => panic!("the wrong manifest key never walks"),
        };
        assert_eq!(detail, "checkpoint-walk/witness-key-mismatch");
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // (f) THE WRONG SHARD KEY: the deterministic key of the
        // WRONG shard index.
        let (root, mut medium, _founding, snap1, snap1_bytes) = arm_base("wrong-shard-key");
        let (witness_mk, _manifest_object, shard_object) = arm_witness(snap1, &snap1_bytes);
        let record = write_record_with_object(
            &mut medium,
            snap1,
            Some(reclamation_witness_shard_idempotency_key(&witness_mk, 1)),
            &shard_object,
            20,
        );
        let detail = match walk_checkpoint_range(&mut medium, record) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the wrong shard key refuses, got {other:?}"),
            Ok(_) => panic!("the wrong shard key never walks"),
        };
        assert_eq!(detail, "checkpoint-walk/witness-key-mismatch");
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // (g) THE INCORRECT REVISION SHAPE: a regular record with
        // TWO content references (the manifest object plus a second
        // payload chosen to sort after it, so the walk's first
        // reference is the witness object).
        let (root, mut medium, _founding, snap1, snap1_bytes) = arm_base("two-refs");
        let (_mk, manifest_object, _shard_object) = arm_witness(snap1, &snap1_bytes);
        let mut counter = 0u32;
        let second = loop {
            let candidate = StoredObject::new({
                let mut enc = engine_core::Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(format!("a1r-two-refs-{counter}").as_bytes());
                enc.into_bytes()
            })
            .unwrap();
            if candidate.hash() > manifest_object.hash() {
                break candidate;
            }
            counter += 1;
        };
        let head = snap1;
        let epoch = top_epoch(&mut medium, head);
        let record = RevisionRecord::regular(head, epoch, 20)
            .with_content_refs(vec![manifest_object.hash(), second.hash()]);
        std::fs::write(
            medium.object_path_pub(&manifest_object.hash()),
            manifest_object.bytes(),
        )
        .unwrap();
        std::fs::write(medium.object_path_pub(&second.hash()), second.bytes()).unwrap();
        std::fs::write(
            medium.object_path_pub(&record.id()),
            record.canonical_bytes(),
        )
        .unwrap();
        let detail = match walk_checkpoint_range(&mut medium, record.id()) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the two-reference shape refuses, got {other:?}"),
            Ok(_) => panic!("the two-reference shape never walks"),
        };
        assert_eq!(detail, "checkpoint-walk/witness-shape-mismatch");
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // (h) THE MISSING CONTENT: the record references an object
        // identity that was never stored.
        let (root, mut medium, _founding, snap1, _) = arm_base("missing-content");
        let fabricated = engine_persist::hash::hash_object(b"a1r-never-stored-object");
        let epoch = top_epoch(&mut medium, snap1);
        let record = RevisionRecord::regular(snap1, epoch, 20).with_content_refs(vec![fabricated]);
        std::fs::write(
            medium.object_path_pub(&record.id()),
            record.canonical_bytes(),
        )
        .unwrap();
        assert!(walk_checkpoint_range(&mut medium, record.id()).is_err());
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        // (i) THE CORRUPTED CONTENT: different bytes written at the
        // valid manifest object's content-addressed path.
        let (root, mut medium, _founding, snap1, snap1_bytes) = arm_base("corrupted-content");
        let (_mk, manifest_object, _shard_object) = arm_witness(snap1, &snap1_bytes);
        let record = write_record_with_object(&mut medium, snap1, None, &manifest_object, 20);
        std::fs::write(
            medium.object_path_pub(&manifest_object.hash()),
            b"a1r-corrupted-witness-content",
        )
        .unwrap();
        assert!(walk_checkpoint_range(&mut medium, record).is_err());
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
}
