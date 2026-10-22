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

use engine_core::wire::MAX_COLLECTION_COUNT;
use engine_core::{
    Decoder, Encoder, EventClass, EventId, Field, LodLevel, ObligationView, SaveError, Spine, Tick,
};

use engine_persist::{
    AppendOutcome, AuthorityMode, AwardCheckpoint, CanonicalMedium, CaptureReceipt, ClassifiedSpan,
    CommitPipeline, ConsequenceEnvelope, ConsequenceId, CounterValues, Coverage, DurabilityClass,
    FrontierEntry, LocalFileMedium, OccurrenceSpan, PipelineError, RevisionRecord, SignalLane,
    SourceBaseline, SourcePosition, T1Journal,
};

use crate::npc::{NPC_COUNT, NpcWorld, POSITION_DISCARDED, REGION_COUNT, REGION_GRID};
use crate::region::{
    CaptureStatus, RegionCheckpoint, RegionDirector, RegionEntry, gather_region_entries, lod_byte,
    read_flag, read_lod_level,
};

/// One collected obligation — the owned form of a collected
/// [`ObligationView`] (the plan is journalable, so it carries owned
/// data and never borrows the spine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanObligation {
    pub id: EventId,
    pub due: Tick,
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
    /// The plan blob's versioned domain.
    pub const DOMAIN: &'static str = "engine-slice/deactivation-plan/1";

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
}

impl From<PipelineError> for TransitionError {
    fn from(error: PipelineError) -> Self {
        TransitionError::JournalFailed(error)
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
    pipeline: CommitPipeline<LocalFileMedium>,
}

impl SessionCrossing {
    /// Opens (or joins) the campaign store: the B2 medium over
    /// `store_dir` — founded with a standalone authority record on
    /// first open, so every confirmation carries the honest
    /// `T3-local (standalone)` label — joined with the caller's
    /// journal in one pipeline. The caller constructs the journal (a
    /// plain open, or a fault-injected open for the failure matrix).
    pub fn open(
        store_dir: &Path,
        journal: T1Journal,
        campaign: &str,
    ) -> Result<Self, PipelineError> {
        let mut medium = LocalFileMedium::open(store_dir.to_path_buf())?;
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
        Ok(SessionCrossing { pipeline })
    }

    /// The six-phase transactional deactivation (§5.5.1/§5.5.2), over
    /// the real simulation state:
    ///
    /// 1. **Validate** — the region is live (a Dormant target from a
    ///    live level; the caller's presence decision).
    /// 2. **Determine** — the [`DeactivationPlan`] from read-only
    ///    sources only.
    /// 3. **Persist T1** — ONE journal transaction: the capture
    ///    envelopes (one per captured NPC — the award form, uniform
    ///    `Captured` units) plus the complete plan as the plan blob.
    /// 4. **Verify** — the receipt exists only after the journal's
    ///    fsync (the B3 contract); an ambiguous write was reconciled
    ///    by reread+verify inside the capture.
    /// 5. **Install** — the staged prepare/install bundle
    ///    ([`install_deactivation`]) at the same tick boundary.
    ///
    /// A pre-durable failure (phases 3–4) is a retryable soft abort —
    /// the pre-transition state stands. A phase-5 defensive failure is
    /// the fail-closed halt signal.
    pub fn deactivate(
        &mut self,
        spine: &mut Spine<NpcWorld>,
        director: &mut RegionDirector,
        region: usize,
        now: Tick,
    ) -> Result<CaptureReceipt, TransitionError> {
        self.transition(spine, director, region, now, |_, _| {})
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
    ) -> Result<CaptureReceipt, TransitionError> {
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
    ) -> Result<CaptureReceipt, TransitionError> {
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
        let plan_bytes = plan.canonical_bytes().map_err(PipelineError::Format)?;
        // Phase 3 — persist T1: ONE transaction — the capture
        // envelopes plus the plan blob. The envelopes stage in
        // ConsequenceId-sorted order (one transaction covers one
        // region and episode, so npc order IS the identity order —
        // the journal's canonical discipline: capture records land in
        // input order and the commit's identity list is written
        // sorted, validated equal at every scan).
        let mut captures = plan.captures.clone();
        captures.sort_by_key(|(npc, _)| *npc);
        for (npc, delta) in &captures {
            let envelope = capture_envelope(region, episode, *npc, *delta, now)
                .map_err(PipelineError::Format)?;
            self.pipeline.stage(envelope)?;
        }
        let receipt = self.pipeline.capture(
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
        )?;
        // Phase 4 — verify: the receipt is post-fsync by contract.
        gap(spine, director);
        // Phase 5 — install (the staged bundle).
        install_deactivation(spine, director, &plan)?;
        Ok(receipt)
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
