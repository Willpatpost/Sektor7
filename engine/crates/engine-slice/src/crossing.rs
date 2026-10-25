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

use engine_core::wire::{MAX_BYTE_LEN, MAX_COLLECTION_COUNT, MAX_TAG_LEN};
use engine_core::{
    CompactionError, Decoder, Diagnostics, Encoder, Event, EventClass, EventId, Field, HistoryKind,
    Level, LodLevel, ObligationView, SaveError, Schedule, Spine, Tick,
};

use engine_persist::{
    AppendOutcome, AuthorityMode, AwardCheckpoint, AwardState, CHECKPOINT_METADATA_DOMAIN,
    CONSEQUENCE_BATCH_DOMAIN, CanonicalMedium, CaptureReceipt, CheckpointMetadata, ClassifiedSpan,
    CommitOutcome, CommitPipeline, CommittedTransaction, ConfirmedEvidence, ConsequenceEnvelope,
    ConsequenceId, CounterValues, Coverage, DurabilityClass, FrontierEntry, Hash,
    JOURNAL_ACTIVE_SUFFIX, JournalRotation, JournalScan, KeyPhase, MediumError, OccurrenceSpan,
    PipelineError, RECLAMATION_LEDGER_DOMAIN, ReclaimUnit, ReclaimedIndexEntry, ReclamationLedger,
    Reconciliation, RecordedCancellation, RecoveryAnchor, RecoveryError, RevisionRecord,
    SignalLane, SkipEntry, SourceBaseline, SourcePosition, StoredObject, T1Journal, TailState,
    TransactionId, active_journal_path, chain_range_digest, compute_digest_seed,
    publish_checkpoint_mk, publish_reclamation_ledger, reconcile, rotate_journal,
};

use crate::npc::{NPC_COUNT, NpcWorld, POSITION_DISCARDED, REGION_COUNT, REGION_GRID};
use crate::region::{
    CaptureStatus, LaneSummaryError, RegionCheckpoint, RegionDirector, RegionEntry,
    gather_region_entries, lod_byte, read_flag, read_lod_level,
};
use crate::store::{SessionStore, StoreLocation};

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
pub struct SessionCrossing {
    pipeline: CommitPipeline<SessionStore>,
    /// The campaign's canonical store location — fresh medium handles
    /// for the snapshot publications and the recovery's chain walk
    /// (each provider's own coordination is per-operation, so
    /// additional handles serialize safely alongside the pipeline's
    /// own).
    store: StoreLocation,
    /// The campaign's journal ROOT — the single authoritative
    /// active-path location (the review corrective F1): every
    /// rotation publishes its pointer at `<root>.active`, and the
    /// recovery and the backup resolve the active journal from that
    /// one root, across any number of successive rotations. Distinct
    /// from the pipeline's ACTIVE journal path after a rotation (the
    /// pipeline holds the rotated generation; the root never moves).
    journal_root: PathBuf,
}

impl SessionCrossing {
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

    /// The provider-aware constructor (PP-B1's integration seam): the
    /// same founding-and-joining semantics over the explicit store
    /// location — the legacy file root or the SQLite campaign
    /// database. **No silent selection ever occurs**: a database at a
    /// fresh path is unfounded until the founding append runs (the
    /// same first-open semantics as the file medium); an existing
    /// campaign at the location is joined, never re-founded.
    pub fn open_at(
        location: &StoreLocation,
        journal: T1Journal,
        campaign: &str,
    ) -> Result<Self, PipelineError> {
        let mut medium = SessionStore::open(location)?;
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
        let journal_root = journal.path().to_path_buf();
        let pipeline = CommitPipeline::open(medium, journal)?;
        Ok(SessionCrossing {
            pipeline,
            store: location.clone(),
            journal_root,
        })
    }

    /// The graceful lifecycle boundary (PP-B1): the ratified `TRUNCATE`
    /// checkpoint for the SQLite provider — a no-op for the file
    /// medium (its every publication already fsyncs through the
    /// nine-step contract). Typed failures, never swallowed.
    pub fn checkpoint(&mut self) -> Result<(), MediumError> {
        SessionStore::open(&self.store)?.checkpoint()
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
        let bytes = save_session(spine, director).map_err(PipelineError::Format)?;
        let object = StoredObject::new(bytes)?;
        let snapshot_hash = object.hash();
        // A fresh medium handle (each provider's coordination is
        // per-operation; the pipeline's own handle is idle between its
        // operations).
        let mut medium = SessionStore::open(&self.store)?;
        let head = medium.head()?.ok_or(PipelineError::UnfoundedMedium)?;
        let parent_epoch = medium.read_record(&head)?.epoch;
        let record = RevisionRecord::regular(head, parent_epoch, spine.clock().as_u64())
            .with_idempotency_key(snapshot_hash.as_bytes().to_vec())
            .with_content_refs(vec![snapshot_hash]);
        match medium.append(&record, std::slice::from_ref(&object), Some(head))? {
            AppendOutcome::Accepted { id, .. } => Ok(id),
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
        let mut medium = SessionStore::open(&self.store)?;
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
        // THE CHAIN-VERIFIED ANCHOR: the newest session snapshot's
        // director state (a compaction the operator has not published
        // is not subsumption evidence — the anchor on the chain is).
        let Some((_anchor_spine, anchor_director)) = latest_snapshot_at(&self.store)? else {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "journal-rotation/no-anchor",
            });
        };
        let boundary = anchor_director
            .ledger()
            .summarized()
            .map(|summary| summary.boundary);
        let subsumed = |identity: &ConsequenceId| {
            boundary.is_some_and(|boundary| identity.episode <= boundary[identity.region as usize])
        };
        // THE CLASSIFICATION (§4.4): retire iff canceled, or
        // actionable with every identity confirmed AND subsumed
        // below the verified anchor's boundary.
        let journal = self.pipeline.journal();
        let mut retained: Vec<CommittedTransaction> = Vec::new();
        let mut retired: Vec<TransactionId> = Vec::new();
        for transaction in journal.transactions() {
            let canceled = journal.is_canceled(transaction.id);
            let settled = transaction
                .envelopes
                .iter()
                .all(|envelope| self.pipeline.is_confirmed(envelope.id) && subsumed(&envelope.id));
            if canceled || settled {
                retired.push(transaction.id);
            } else {
                retained.push(transaction.clone());
            }
        }
        // Cancellations survive while their target survives.
        let retained_ids: BTreeSet<TransactionId> =
            retained.iter().map(|transaction| transaction.id).collect();
        let retained_cancellations: Vec<RecordedCancellation> = journal
            .cancellations()
            .iter()
            .filter(|cancellation| retained_ids.contains(&cancellation.target))
            .cloned()
            .collect();
        // THE MECHANISM: write + scan-verify + flip — the pointer
        // ALWAYS lands at THE CAMPAIGN ROOT (the review corrective
        // F1: one stable authority across repeated rotations — the
        // recovery and the backup resolve the active journal from
        // the root's single pointer, never from a pointer published
        // beside a rotated generation).
        let root = self.journal_root.clone();
        let rotation = rotate_journal(&root, &retained, &retained_cancellations, &retired)
            .map_err(PipelineError::Journal)?;
        // THE SWAP: this crossing's pipeline reopens over the new
        // journal — the keys reseeded from it, the confirmed set
        // re-derived from the chain.
        let journal = T1Journal::open(&rotation.new_path).map_err(PipelineError::Journal)?;
        let medium = SessionStore::open(&self.store).map_err(PipelineError::Medium)?;
        self.pipeline = CommitPipeline::open(medium, journal)?;
        Ok(rotation)
    }

    /// The journal this crossing's pipeline holds (diagnostics — the
    /// rotation's swap and the recovery's pointer resolution are
    /// observable here).
    pub fn pipeline_journal_path(&self) -> &Path {
        self.pipeline.journal().path()
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
        let mut medium = SessionStore::open(&self.store).map_err(PipelineError::Medium)?;
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
                        .reclaim_pending(&ledger.units)
                        .map_err(PipelineError::Medium)?
                        || journals_pending
                    {
                        target = Some((*candidate, sk, metadata, Some((record_id, ledger))));
                        break;
                    }
                    // Completed — the manifest fully satisfied — skip.
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
                for entry in &indexed {
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
        let removed_objects = medium
            .reclaim(&ledger.units)
            .map_err(PipelineError::Medium)?;
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
        if latest_snapshot_at(&self.store)?.is_none() {
            return Err(TransitionError::InstallIncompatible {
                region: 0,
                detail: "reclaim/anchor-lost",
            });
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
        Ok(self.pipeline.commit_canonical()?)
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
    ) -> Result<(Spine<NpcWorld>, RegionDirector, SessionCrossing), TransitionError> {
        Self::recover_at(
            &StoreLocation::Directory(store_dir.to_path_buf()),
            journal_path,
            campaign,
        )
    }

    /// The provider-aware recovery (PP-B1): the same reconstruction
    /// over the explicit store location — the same composition, the
    /// same contracts, the same fail-closed behavior.
    pub fn recover_at(
        location: &StoreLocation,
        journal_path: &Path,
        campaign: &str,
    ) -> Result<(Spine<NpcWorld>, RegionDirector, SessionCrossing), TransitionError> {
        // 1. The snapshot — fail closed on a recognizable-but-invalid
        //    newest session object (corruption of the newest
        //    authoritative state is never walked past).
        let Some((mut spine, mut director)) = latest_snapshot_at(location)? else {
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
        // A POINTER-RESOLVED journal that does not exist is a broken
        // rotation state — fail closed, never the empty fallback. The
        // absent-LEGACY case (no pointer present) keeps the accepted
        // B3 coherence semantics: an empty scan, the chain and the
        // snapshot carrying the truth.
        let pointer_present = active_journal != journal_path;
        let scan = match T1Journal::scan_file(&active_journal) {
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
        let journal = T1Journal::open(&active_journal).map_err(PipelineError::Journal)?;
        let mut crossing = SessionCrossing::open_at(location, journal, campaign)?;
        // The campaign journal ROOT (the review corrective F1): the
        // pointer's authoritative location — NOT the resolved active
        // journal the pipeline holds. Repeated rotations and the
        // recovery both resolve from this one root.
        crossing.journal_root = journal_path.to_path_buf();
        let mut restored = vec![false; NPC_COUNT];
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
/// medium's nine-step append (the verified snapshot boundary).
pub const SESSION_SNAPSHOT_DOMAIN: &str = "engine-slice/session-snapshot/1";

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
/// codecs' gates; the blob consumed exactly).
pub fn restore_session(bytes: &[u8]) -> Result<(Spine<NpcWorld>, RegionDirector), SaveError> {
    const SECTION: &str = "session-snapshot";
    let mut dec = Decoder::new(bytes);
    dec.read_domain(SESSION_SNAPSHOT_DOMAIN)?;
    let spine_bytes = dec.read_bytes(MAX_BYTE_LEN, SECTION)?;
    let director_bytes = dec.read_bytes(MAX_BYTE_LEN, SECTION)?;
    dec.finish()?;
    let spine = Spine::<NpcWorld>::restore(spine_bytes, Diagnostics::new(Level::Off))?;
    let mut director_dec = Decoder::new(director_bytes);
    let director = RegionDirector::restore(&mut director_dec)?;
    Ok((spine, director))
}

/// The latest session snapshot on the chain: walk from HEAD through
/// the parents, reading each revision's first content object. The
/// FIRST object recognizable as a session snapshot (the
/// domain-family tag) is the NEWEST authoritative session state: it
/// must restore — a recognizable-but-invalid newest session object
/// fails CLOSED (corruption of the newest authoritative state is
/// never walked past to an older snapshot, which would resurrect
/// stale authoritative state). The founding record carries no
/// content; the walk is bounded by the chain length.
fn latest_snapshot_at(
    location: &StoreLocation,
) -> Result<Option<(Spine<NpcWorld>, RegionDirector)>, TransitionError> {
    let mut medium = SessionStore::open(location)?;
    let mut cursor = medium.head()?;
    while let Some(id) = cursor {
        let record = medium.read_record(&id)?;
        let Some(hash) = record.content_refs.first() else {
            cursor = record.parent;
            continue;
        };
        let bytes = medium.read_object(hash)?;
        if is_session_object(&bytes) {
            return restore_session(&bytes)
                .map(Some)
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
/// state.
fn is_session_object(bytes: &[u8]) -> bool {
    const FAMILY: &[u8] = b"engine-slice/session-snapshot";
    if bytes.len() < 8 {
        return false;
    }
    let tag_len =
        u64::from_le_bytes(bytes[0..8].try_into().expect("the slice is 8 bytes")) as usize;
    tag_len >= FAMILY.len() && 8 + tag_len <= bytes.len() && &bytes[8..8 + FAMILY.len()] == FAMILY
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
    medium: &mut SessionStore,
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
    medium: &mut SessionStore,
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
/// (PP-B3's Phase B, the review corrective F1): only after the
/// rotation's verified active journal stands — no pointer means no
/// rotation, and the campaign root journal IS the active one (never
/// touched). With the pointer present, every journal file beside
/// the root EXCEPT the current active generation is a stale
/// generation (the root's own file and every prior rotated
/// generation), each removable under the audited retirement — the
/// prior generations never accumulate.
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
    let Some(dir) = root.parent() else {
        return Ok(Vec::new());
    };
    let Some(root_name) = root.file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };
    let mut stale = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // The pointer family and the sidecar locks are handled
            // separately (the pointer must survive to keep resolving
            // the active journal; the locks are removed with their
            // journals by the caller).
            if name.ends_with(JOURNAL_ACTIVE_SUFFIX) || name.ends_with(".lock") {
                continue;
            }
            let is_journal =
                name == root_name || (name.starts_with("journal-") && name.ends_with(".t1"));
            if !is_journal || active_name.as_deref() == Some(name.as_str()) {
                continue;
            }
            stale.push(name);
        }
    }
    stale.sort();
    Ok(stale)
}
