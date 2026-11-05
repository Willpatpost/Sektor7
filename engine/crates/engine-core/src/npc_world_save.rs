//! (V2-W F4, the R15 world-validator authority) The NPC world's save
//! section — the shared, statically linked interpretation of the
//! spine save's world-section wire format.
//!
//! **Why this module exists (the R15 closure):** the authenticated
//! rotation anchor must validate the newest session snapshot's spine
//! under the PRODUCTION world's own rules — a hash-verified object
//! with a valid director summary but an invalid world section is not
//! valid retirement evidence (the R14-A finding), and the validator
//! that authenticates retirement evidence must be the validator
//! belonging to the campaign's actual persisted world schema, never a
//! freely selected caller implementation (the R15 finding: the
//! public generic rotation entry let any external crate substitute a
//! weaker `SaveWorld` that declares the production domain — the
//! bypass was demonstrated through the real public API and is now
//! UNREPRESENTABLE there: the entry takes no world parameter, and
//! the anchor restores through [`NpcSaveWorld`], whose acceptance is
//! exactly this section codec's acceptance).
//!
//! **The parity is by construction, not by mirroring:** the
//! application's [`crate::SaveWorld`] implementation for its
//! `NpcWorld` DELEGATES its restore to
//! [`NpcWorldSection::restore`] (the same function this module
//! defines), and the anchor's spine validation runs the production
//! `Spine::<W>::restore` machinery itself with `W = NpcSaveWorld`
//! (also delegating here). One walk, two consumers — no
//! independently maintained interpretation can drift. This is the
//! same downward move the layout constants and `LodConfig::validated`
//! already made (the V2-W F4 authenticated-retention design): the
//! wire format's anchor-relevant rules live in engine-core, beside
//! the bounds (`NPC_COUNT`, `REGION_GRID`, `POSITION_DISCARDED`)
//! they parameterize; the application keeps its behavior (the
//! `World` event handling and the save-side encode) and the
//! persistence layer's anchor names only this module.
//!
//! The section is the world save's PAYLOAD codec (the bytes inside
//! the spine save's world-section framing — after the domain tag and
//! length prefix, which the spine's generic save/restore owns):
//! [`NpcWorldSection::restore`] consumes exactly the production
//! encoding or fails closed with the production error taxonomy, and
//! [`NpcWorldSection::save`] emits the same sequence (the
//! application's `NpcWorld::save` keeps its own verbatim encoder —
//! byte-identical, pinned by the round-trip parity tests — so no
//! clone is introduced on the save path).

use crate::clock::Tick;
use crate::event::Event;
use crate::fingerprint::Fingerprinter;
use crate::lod::LodLevel;
use crate::spine::{ExecCtx, World};
use crate::wire::{Decoder, Encoder, SaveError, SaveWorld};
use crate::{NPC_COUNT, POSITION_DISCARDED, REGION_COUNT, REGION_GRID};

/// The NPC world save section's domain tag — versioned and stable,
/// the ONE string both the application's `NpcWorld::save` and the
/// anchor's fixed world declare (the spine's generic framing checks
/// it against the restoring world's `SAVE_DOMAIN`).
pub const NPC_WORLD_SAVE_DOMAIN: &str = "engine-slice/npc-world-save/1";

/// The NPC world's save section as plain data: the behavioral
/// configuration plus the authoritative per-NPC state (dense arrays
/// indexed by the stable identity set — ONE count governs every
/// array; the identity set is fixed, so a different count is an
/// incompatible workload, rejected at restore, never a resized
/// restore). The application's `NpcWorld` assembles itself from this
/// section; the anchor's [`NpcSaveWorld`] holds it restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpcWorldSection {
    /// Deterministic workload seed (behavioral identity).
    pub seed: u64,
    /// The near-region classification as a `REGION_COUNT`-bit mask
    /// (validated bounded at restore; the application expands it).
    pub near_mask: u64,
    /// Base activity period in ticks.
    pub activity_period: u64,
    /// The cadence multiplier at Simplified LOD.
    pub simplify_factor: u64,
    /// The commitment window at EventDriven LOD.
    pub commitment_window: u64,
    /// Whether the region-LOD handler path is enabled.
    pub region_lods_enabled: bool,
    /// Executed activity count per NPC — ACTUAL executions only.
    pub steps: Vec<u64>,
    /// Routine progression cursor per NPC (with discard sentinels
    /// while dormant).
    pub routine_cursor: Vec<u64>,
    /// Per-NPC position on the region-local grid (or the
    /// discarded-position sentinel while dormant).
    pub positions: Vec<(u8, u8)>,
    /// Per-NPC represented-through boundary.
    pub represented_through: Vec<Tick>,
    /// Real-execution milestones per NPC (irreversible
    /// consequences).
    pub milestones: Vec<u64>,
    /// Uncommitted consequential signals per NPC (scratch).
    pub pending_signals: Vec<u64>,
    /// Committed consequential signals per NPC.
    pub credits: Vec<u64>,
    /// The handler's current LOD view per region.
    pub region_lods: [LodLevel; REGION_COUNT],
}

impl NpcWorldSection {
    /// Encodes the section — the same sequence the application's
    /// `NpcWorld::save` writes (the production encoder; this mirror
    /// exists so the fixed world's [`SaveWorld::save`] is total and
    /// the section round-trips under its own codec — the parity tests
    /// pin the two encoders together).
    pub fn save(&self, enc: &mut Encoder) {
        enc.write_u64(self.seed);
        enc.write_u64(self.near_mask);
        enc.write_u64(self.activity_period);
        enc.write_u64(self.simplify_factor);
        enc.write_u64(self.commitment_window);
        enc.write_u8(u8::from(self.region_lods_enabled));
        enc.write_count(NPC_COUNT);
        for step in &self.steps {
            enc.write_u64(*step);
        }
        for cursor in &self.routine_cursor {
            enc.write_u64(*cursor);
        }
        for position in &self.positions {
            enc.write_u8(position.0);
            enc.write_u8(position.1);
        }
        for boundary in &self.represented_through {
            enc.write_tick(*boundary);
        }
        for milestone in &self.milestones {
            enc.write_u64(*milestone);
        }
        for signal in &self.pending_signals {
            enc.write_u64(*signal);
        }
        for credit in &self.credits {
            enc.write_u64(*credit);
        }
        for lod in &self.region_lods {
            enc.write_u8(match lod {
                LodLevel::Full => 0,
                LodLevel::Simplified => 1,
                LodLevel::EventDriven => 2,
                LodLevel::Dormant => 3,
            });
        }
    }

    /// Restores the section (fail-closed): the exact production walk
    /// — every count bounded before allocation, every discriminant
    /// typed, the fixed identity set, the grid-position bounds with
    /// the dormant-discard sentinel, the near-mask bound — consuming
    /// exactly the production encoding or returning the production
    /// typed error. Malformed input is never repaired.
    pub fn restore(dec: &mut Decoder) -> Result<Self, SaveError> {
        const SECTION: &str = "npc-world-save";
        let seed = dec.read_u64()?;
        let near_mask = dec.read_u64()?;
        let activity_period = dec.read_u64()?;
        let simplify_factor = dec.read_u64()?;
        let commitment_window = dec.read_u64()?;
        let region_lods_enabled = match dec.read_u8()? {
            0 => false,
            1 => true,
            discriminant => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(discriminant),
                });
            }
        };
        // The fixed identity set: the count is bounded by the
        // documented workload size and the remaining bytes before any
        // allocation trusts it, and must equal NPC_COUNT exactly — a
        // different identity set is an incompatible workload, never a
        // silently resized restore.
        let npc_count = dec.read_count(NPC_COUNT as u64, SECTION, "npc-count")?;
        if npc_count != NPC_COUNT as u64 {
            return Err(SaveError::Integrity("npc-world-save/npc-count"));
        }
        let mut steps = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            steps.push(dec.read_u64()?);
        }
        let mut routine_cursor = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            routine_cursor.push(dec.read_u64()?);
        }
        // Grid positions carry the dormant-discard sentinel
        // (`POSITION_DISCARDED` — the reconstructible representation
        // is GONE, not merely flagged); every other position must lie
        // on the region-local grid (the bounds invariant).
        let mut positions = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            let x = dec.read_u8()?;
            let y = dec.read_u8()?;
            if (x, y) != POSITION_DISCARDED && (x >= REGION_GRID || y >= REGION_GRID) {
                return Err(SaveError::Integrity("npc-world-save/position-bounds"));
            }
            positions.push((x, y));
        }
        let mut represented_through = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            represented_through.push(dec.read_tick()?);
        }
        let mut milestones = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            milestones.push(dec.read_u64()?);
        }
        let mut pending_signals = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            pending_signals.push(dec.read_u64()?);
        }
        let mut credits = Vec::with_capacity(NPC_COUNT);
        for _ in 0..NPC_COUNT {
            credits.push(dec.read_u64()?);
        }
        let mut region_lods = [LodLevel::Full; REGION_COUNT];
        for lod in &mut region_lods {
            *lod = match dec.read_u8()? {
                0 => LodLevel::Full,
                1 => LodLevel::Simplified,
                2 => LodLevel::EventDriven,
                3 => LodLevel::Dormant,
                discriminant => {
                    return Err(SaveError::Enum {
                        section: SECTION,
                        discriminant: u64::from(discriminant),
                    });
                }
            };
        }
        // The near-region mask is a REGION_COUNT-bit mask — set bits
        // above it are not this workload's configuration.
        if near_mask >> REGION_COUNT != 0 {
            return Err(SaveError::Integrity("npc-world-save/near-mask"));
        }
        Ok(NpcWorldSection {
            seed,
            near_mask,
            activity_period,
            simplify_factor,
            commitment_window,
            region_lods_enabled,
            steps,
            routine_cursor,
            positions,
            represented_through,
            milestones,
            pending_signals,
            credits,
            region_lods,
        })
    }
}

/// The fixed world the authenticated anchor restores through: a
/// [`World`]/[`SaveWorld`] implementation whose section acceptance is
/// EXACTLY [`NpcWorldSection::restore`]'s (the same function the
/// application's production `NpcWorld::restore` delegates to) — so
/// `Spine::<NpcSaveWorld>::restore` accepts precisely the spine
/// saves the production `Spine::<NpcWorld>::restore` accepts, and
/// the anchor needs no caller-supplied world parameter at all. This
/// type exists for VALIDATION only: it never executes (a no-op
/// handler) and never fingerprints; it is never a live game world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpcSaveWorld(pub NpcWorldSection);

impl World for NpcSaveWorld {
    fn execute(&mut self, _event: &Event, _ctx: &mut ExecCtx) {}
    fn fingerprint(&self, _fp: &mut Fingerprinter) {}
}

impl SaveWorld for NpcSaveWorld {
    const SAVE_DOMAIN: &'static str = NPC_WORLD_SAVE_DOMAIN;

    fn save(&self, enc: &mut Encoder) {
        self.0.save(enc);
    }

    fn restore(dec: &mut Decoder) -> Result<Self, SaveError> {
        Ok(NpcSaveWorld(NpcWorldSection::restore(dec)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_section() -> NpcWorldSection {
        let positions = (0..NPC_COUNT)
            .map(|npc| ((npc % 16) as u8, ((npc / 16) % 16) as u8))
            .collect();
        NpcWorldSection {
            seed: 1337,
            near_mask: 0b0000_0011,
            activity_period: 6,
            simplify_factor: 4,
            commitment_window: 12,
            region_lods_enabled: true,
            steps: (0..NPC_COUNT).map(|npc| npc as u64).collect(),
            routine_cursor: (0..NPC_COUNT).map(|npc| (npc / 2) as u64).collect(),
            positions,
            represented_through: vec![Tick(7); NPC_COUNT],
            milestones: (0..NPC_COUNT).map(|npc| (npc / 16) as u64).collect(),
            pending_signals: vec![0; NPC_COUNT],
            credits: (0..NPC_COUNT).map(|npc| (npc / 32) as u64).collect(),
            region_lods: [
                LodLevel::Full,
                LodLevel::Simplified,
                LodLevel::EventDriven,
                LodLevel::Dormant,
                LodLevel::Full,
                LodLevel::Simplified,
                LodLevel::EventDriven,
                LodLevel::Dormant,
            ],
        }
    }

    fn encode(section: &NpcWorldSection) -> Vec<u8> {
        let mut enc = Encoder::new();
        section.save(&mut enc);
        enc.into_bytes()
    }

    #[test]
    fn the_section_round_trips_under_its_own_codec() {
        let section = sample_section();
        let bytes = encode(&section);
        let mut dec = Decoder::new(&bytes);
        let restored = NpcWorldSection::restore(&mut dec).unwrap();
        dec.finish().unwrap();
        assert_eq!(restored, section);
    }

    #[test]
    fn the_fixed_world_declares_the_production_domain_and_delegates() {
        // The acceptance identity: NpcSaveWorld::restore is the
        // section codec, and the domain is the ONE string — the
        // spine's world-section framing accepts it exactly where the
        // production world's does.
        assert_eq!(NpcSaveWorld::SAVE_DOMAIN, NPC_WORLD_SAVE_DOMAIN);
        assert_eq!(NPC_WORLD_SAVE_DOMAIN, "engine-slice/npc-world-save/1");
        let bytes = encode(&sample_section());
        let mut dec = Decoder::new(&bytes);
        let world = NpcSaveWorld::restore(&mut dec).unwrap();
        dec.finish().unwrap();
        assert_eq!(world.0, sample_section());
    }

    #[test]
    fn a_truncated_section_fails_closed() {
        // The R15 adversarial shape: an 8-byte payload (well-formed
        // as a weak blob, structurally short for the production
        // walk) fails the section codec — the production error the
        // bypass exploited.
        let mut enc = Encoder::new();
        enc.write_u64(0);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(NpcWorldSection::restore(&mut dec).is_err());
    }

    #[test]
    fn an_incompatible_identity_count_is_refused_never_resized() {
        // The count rewrite: the production prefix with the count
        // reading NPC_COUNT + 1 — an incompatible workload, refused.
        let section = sample_section();
        let mut enc = Encoder::new();
        enc.write_u64(section.seed);
        enc.write_u64(section.near_mask);
        enc.write_u64(section.activity_period);
        enc.write_u64(section.simplify_factor);
        enc.write_u64(section.commitment_window);
        enc.write_u8(1);
        enc.write_count(NPC_COUNT + 1);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(NpcWorldSection::restore(&mut dec).is_err());
    }

    #[test]
    fn an_off_grid_position_is_refused() {
        let mut section = sample_section();
        section.positions[5] = (REGION_GRID, 0);
        let bytes = encode(&section);
        let mut dec = Decoder::new(&bytes);
        assert!(NpcWorldSection::restore(&mut dec).is_err());
        // The dormant-discard sentinel is the one off-grid exemption.
        section.positions[5] = POSITION_DISCARDED;
        let bytes = encode(&section);
        let mut dec = Decoder::new(&bytes);
        assert!(NpcWorldSection::restore(&mut dec).is_ok());
    }

    #[test]
    fn unknown_discriminants_and_mask_overflow_fail_closed() {
        let mut section = sample_section();
        section.region_lods[2] = LodLevel::Full;
        let mut bytes = encode(&section);
        // The last byte is the final region's LOD discriminant.
        *bytes.last_mut().unwrap() = 9;
        let mut dec = Decoder::new(&bytes);
        assert!(NpcWorldSection::restore(&mut dec).is_err());

        let mut section = sample_section();
        section.near_mask = 1 << REGION_COUNT;
        let bytes = encode(&section);
        let mut dec = Decoder::new(&bytes);
        assert!(NpcWorldSection::restore(&mut dec).is_err());

        // The enabled-flag discriminant.
        let section = sample_section();
        let mut bytes = encode(&section);
        // The flag is the 6th field: 5 u64s (40 bytes) then the u8.
        bytes[40] = 7;
        let mut dec = Decoder::new(&bytes);
        assert!(NpcWorldSection::restore(&mut dec).is_err());
    }
}
