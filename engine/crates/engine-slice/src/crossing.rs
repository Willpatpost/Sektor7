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

use engine_core::wire::MAX_COLLECTION_COUNT;
use engine_core::{
    Decoder, Encoder, EventClass, EventId, Field, LodLevel, ObligationView, SaveError, Tick,
};

use crate::npc::{NPC_COUNT, NpcWorld, POSITION_DISCARDED, REGION_COUNT, REGION_GRID};
use crate::region::{RegionEntry, gather_region_entries, lod_byte, read_flag, read_lod_level};

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
