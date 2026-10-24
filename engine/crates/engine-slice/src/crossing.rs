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

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use engine_core::wire::{MAX_BYTE_LEN, MAX_COLLECTION_COUNT};
use engine_core::{
    Decoder, Diagnostics, Encoder, Event, EventClass, EventId, Field, Level, LodLevel,
    ObligationView, SaveError, Schedule, Spine, Tick,
};

use engine_persist::{
    AppendOutcome, AuthorityMode, AwardCheckpoint, AwardState, CanonicalMedium, CaptureReceipt,
    ClassifiedSpan, CommitOutcome, CommitPipeline, ConsequenceEnvelope, ConsequenceId,
    CounterValues, Coverage, DurabilityClass, FrontierEntry, Hash, JournalScan, KeyPhase,
    MediumError, OccurrenceSpan, PipelineError, RecoveryAnchor, RecoveryError, RevisionRecord,
    SignalLane, SourceBaseline, SourcePosition, StoredObject, T1Journal, TailState, reconcile,
};

use crate::npc::{NPC_COUNT, NpcWorld, POSITION_DISCARDED, REGION_COUNT, REGION_GRID};
use crate::region::{
    CaptureStatus, RegionCheckpoint, RegionDirector, RegionEntry, gather_region_entries, lod_byte,
    read_flag, read_lod_level,
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
        let pipeline = CommitPipeline::open(medium, journal)?;
        Ok(SessionCrossing {
            pipeline,
            store: location.clone(),
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
        //    chain and the snapshot carry the truth.
        let scan = match T1Journal::scan_file(journal_path) {
            Ok(scan) => scan,
            Err(engine_persist::JournalError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
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
        let journal = T1Journal::open(journal_path).map_err(PipelineError::Journal)?;
        let mut crossing = SessionCrossing::open_at(location, journal, campaign)?;
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
        //    own per-identity state (journal-independent).
        let confirmed = crossing.pipeline.confirmed_evidence();
        let anchor = build_anchor(&spine, &director)?;
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
/// NPC (both session-origin — the snapshot's internal coherence; a
/// divergence is the fail-closed halt).
fn build_anchor(
    spine: &Spine<NpcWorld>,
    director: &RegionDirector,
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
    // The coherence: the world's credits == the sum of the ledger's
    // applied amounts per NPC — both session-origin (the snapshot's
    // internal coherence; a divergence is the halt).
    let mut applied_amounts = vec![0u64; NPC_COUNT];
    for (id, amount) in director.ledger().applied_entries() {
        applied_amounts[id.npc as usize] += amount;
    }
    for (npc, amounts) in applied_amounts.iter().enumerate() {
        if *amounts != spine.world().credits()[npc] {
            return Err(TransitionError::InstallIncompatible {
                region: npc % REGION_COUNT,
                detail: "applied-amounts-conservation",
            });
        }
    }
    Ok(anchor)
}
