//! (V2-W F4, the authenticated rotation retention) The shared,
//! statically linked, version-aware session-snapshot anchor
//! decoding — the canonical chain-side derivation of the per-region
//! retirement boundary from the NEWEST session snapshot.
//!
//! The session snapshot's container and the region-director save
//! section are wire formats shared between the application layer
//! (engine-slice, which owns the full game-side restore) and this
//! persistence layer (whose rotation entry must derive the retention
//! boundary WITHOUT trusting a caller-supplied value). This module
//! is the ONE interpretation of those wire formats' anchor-relevant
//! parts: the same domain constants, the same container framing, the
//! same strict field-by-field validation as the accepted
//! `RegionDirector::restore` (every count bounded before allocation,
//! every discriminant typed, the finite-float rule, the canonical
//! strict-ascending ledger order, the identity and grid bounds, and
//! the two compacted-lane gates — the retained-lanes disjointness
//! and the boundary-frontier rule). The game layout constants
//! (REGION_COUNT, NPC_COUNT, REGION_GRID, POSITION_DISCARDED) are
//! authoritative in engine-core, so this decoder and the
//! application read ONE constant — no independently maintained
//! interpretation can drift.
//!
//! **The authority derivation** (`latest_session_anchor`): the walk
//! descends from the canonical HEAD through the parents, reading
//! each revision's first content object through the medium's
//! HASH-VERIFYING read; the FIRST object recognizable as a session
//! snapshot (the domain FAMILY tag) is the NEWEST authoritative
//! session state — exactly the recovery's selection rule (a
//! recognizable-but-invalid newest snapshot fails CLOSED, never
//! falling back to an older one). The recognized object's domain
//! must be EXACTLY the supported version (`…/1`) — a family match
//! with a newer version is an UNSUPPORTED snapshot, refused typed.
//! The container decode splits the spine section (read and
//! discarded — bounded) from the director section (validated
//! field-by-field), and the boundary derives EXCLUSIVELY from the
//! decoded lane-summary evidence. An uncompacted ledger (v3, no
//! summary) yields NO boundary — the rotation then retires only
//! what it can independently prove safe (cancellations).

use engine_core::wire::{MAX_BYTE_LEN, MAX_COLLECTION_COUNT, MAX_TAG_LEN};
use engine_core::{Decoder, NPC_COUNT, POSITION_DISCARDED, REGION_COUNT, REGION_GRID, SaveError};

use crate::consequence::ConsequenceId;
use crate::medium::CanonicalMedium;

/// The session snapshot's versioned domain (the application's
/// snapshot format — the wire format this module shares down for the
/// authenticated anchor).
pub const SESSION_SNAPSHOT_DOMAIN: &str = "engine-slice/session-snapshot/1";

/// The session-snapshot domain FAMILY (any version — the
/// recognition prefix; the supported version is
/// [`SESSION_SNAPSHOT_DOMAIN`]).
const SESSION_SNAPSHOT_FAMILY: &[u8] = b"engine-slice/session-snapshot";

/// The maximum chain descent for the newest-snapshot walk (the
/// established bounded-walk discipline).
const MAX_DESCENT: u64 = 1 << 20;

/// The authenticated session anchor: the per-region
/// summarized-episode retirement boundary derived EXCLUSIVELY from
/// the newest canonical session snapshot's decoded lane-summary
/// evidence (`None` when the snapshot's ledger is uncompacted — v3,
/// no summary — in which case nothing is subsumption-retirable).
#[derive(Debug, Clone)]
pub struct SessionAnchor {
    /// The per-region summarized-episode boundary (region-indexed;
    /// the subsumption rule: an identity is subsumed iff its episode
    /// is at or below its region's boundary).
    pub boundary: Option<[u64; REGION_COUNT]>,
}

/// The anchor derivation's typed failure: the medium walk's
/// failures, the no-snapshot case, and the snapshot decode's
/// fail-closed refusals.
#[derive(Debug)]
pub enum AnchorError {
    Medium(crate::medium::MediumError),
    /// The chain carries no session snapshot at all (the rotation's
    /// no-anchor refusal — nothing is subsumption-retirable).
    NoAnchor,
    /// The newest recognizable snapshot's domain is a family member
    /// the supported version does not cover (fail closed — never a
    /// fallback to an older snapshot).
    UnsupportedSnapshotVersion,
    /// The newest recognizable snapshot failed its strict decode
    /// (malformed, contradictory, or unreadable evidence — fail
    /// closed).
    CorruptSnapshot,
}

impl From<crate::medium::MediumError> for AnchorError {
    fn from(error: crate::medium::MediumError) -> Self {
        AnchorError::Medium(error)
    }
}

/// Whether a content object is recognizable as a session snapshot:
/// the object's first wire field (the u64-length-prefixed domain
/// string) carries the session-snapshot domain FAMILY — any version
/// (recognition peeks the tag only; the supported version is
/// checked at the decode). THE SAME recognizer the recovery's
/// latest-snapshot selection uses (the selection classes must match
/// exactly).
pub fn is_session_object(bytes: &[u8]) -> bool {
    if bytes.len() < 8 {
        return false;
    }
    let tag_len =
        u64::from_le_bytes(bytes[0..8].try_into().expect("the slice is 8 bytes")) as usize;
    tag_len >= SESSION_SNAPSHOT_FAMILY.len()
        && 8 + tag_len <= bytes.len()
        && &bytes[8..8 + SESSION_SNAPSHOT_FAMILY.len()] == SESSION_SNAPSHOT_FAMILY
}

/// Reads the session-snapshot container framing: the exact supported
/// domain, the spine section's bytes (the game-side restore decodes
/// them; the anchor discards them), the director section's bytes,
/// and the exact-consumption finish. THE ONE container
/// interpretation — the application's `restore_session` and the
/// anchor derivation both use this.
pub fn read_snapshot_container(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>), SaveError> {
    const SECTION: &str = "session-snapshot";
    let mut dec = Decoder::new(bytes);
    dec.read_domain(SESSION_SNAPSHOT_DOMAIN)?;
    let spine_bytes = dec.read_bytes(MAX_BYTE_LEN, SECTION)?;
    let director_bytes = dec.read_bytes(MAX_BYTE_LEN, SECTION)?;
    dec.finish()?;
    Ok((spine_bytes.to_vec(), director_bytes.to_vec()))
}

/// Derives the authenticated session anchor from the canonical
/// chain: the newest session snapshot (the HEAD-descent selection —
/// the FIRST family-recognizable first content object), hash-verified
/// through the medium's read, its domain exactly the supported
/// version, its director section strictly validated, and the
/// boundary derived exclusively from the decoded lane summary.
/// `Ok(None)` when the chain carries no session snapshot at all (the
/// rotation's no-anchor refusal applies).
pub fn latest_session_anchor(
    medium: &mut dyn CanonicalMedium,
) -> Result<Option<SessionAnchor>, AnchorError> {
    let Some(head) = medium.head()? else {
        return Ok(None);
    };
    let mut current = head;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..MAX_DESCENT {
        if !seen.insert(current) {
            return Err(AnchorError::Medium(crate::medium::MediumError::Chain {
                rule: "cycle",
            }));
        }
        let record = medium.read_record(&current)?;
        if let Some(object) = record.content_refs.first().copied() {
            let bytes = medium.read_object(&object)?;
            if is_session_object(&bytes) {
                // THE NEWEST authoritative session snapshot: the
                // domain must be EXACTLY the supported version (a
                // family member with a newer version is unsupported
                // — fail closed, never a fallback).
                let domain = peek_domain(&bytes).ok_or(AnchorError::CorruptSnapshot)?;
                if domain != SESSION_SNAPSHOT_DOMAIN {
                    return Err(AnchorError::UnsupportedSnapshotVersion);
                }
                let (_spine_bytes, director_bytes) = read_snapshot_container(&bytes)
                    .map_err(|_error| AnchorError::CorruptSnapshot)?;
                let boundary = decode_director_anchor(&director_bytes)
                    .map_err(|_error| AnchorError::CorruptSnapshot)?;
                return Ok(Some(SessionAnchor { boundary }));
            }
        }
        let Some(parent) = record.parent else {
            return Ok(None);
        };
        current = parent;
    }
    Err(AnchorError::Medium(crate::medium::MediumError::Chain {
        rule: "session-anchor/descent-bound",
    }))
}

/// Peeks a content object's leading domain tag (the recognition
/// input; no payload parsed).
fn peek_domain(bytes: &[u8]) -> Option<String> {
    Decoder::new(bytes)
        .read_str(MAX_TAG_LEN, "session-snapshot")
        .ok()
        .map(|domain| domain.to_owned())
}

/// The anchor-relevant strict decode of the region-director save
/// section: THE SAME field-by-field validation as the accepted
/// `RegionDirector::restore` (every count bounded before allocation,
/// every discriminant typed, the finite-float rule, the identity and
/// grid bounds, the canonical strict-ascending ledger order, and the
/// two compacted-lane gates — the retained-lanes disjointness and
/// the boundary-frontier rule), collecting the per-region episode
/// counters and the lane-summary boundary. Returns the boundary
/// (`None` for an uncompacted v3 ledger).
fn decode_director_anchor(bytes: &[u8]) -> Result<Option<[u64; REGION_COUNT]>, SaveError> {
    const SECTION: &str = "region-director-save";
    let mut dec = Decoder::new(bytes);
    // The behavioral configuration (validated for structure; the
    // game-side revalidation through `RegionDirector::new` belongs
    // to the full restore — the anchor needs the wire's own values).
    let center_count = dec.read_count(REGION_COUNT as u64, SECTION, "center-count")?;
    if center_count != REGION_COUNT as u64 {
        return Err(SaveError::Integrity("region-director-save/center-count"));
    }
    for _ in 0..center_count {
        read_finite_f64(&mut dec)?;
        read_finite_f64(&mut dec)?;
    }
    for _ in 0..7 {
        read_finite_f64(&mut dec)?;
    }
    dec.read_u64()?; // adaptive_threshold
    // The per-region authoritative states.
    let state_count = dec.read_count(REGION_COUNT as u64, SECTION, "state-count")?;
    if state_count != REGION_COUNT as u64 {
        return Err(SaveError::Integrity("region-director-save/state-count"));
    }
    let mut episodes = [0u64; REGION_COUNT];
    let mut region_index = 0usize;
    while region_index < REGION_COUNT {
        read_lod_level(&mut dec, SECTION)?;
        episodes[region_index] = dec.read_u64()?;
        let checkpoint_present = read_flag(&mut dec, SECTION)?;
        if checkpoint_present {
            dec.read_tick()?;
            read_lod_level(&mut dec, SECTION)?;
            let entry_count = dec.read_count(NPC_COUNT as u64, SECTION, "entry-count")?;
            for _ in 0..entry_count {
                let npc = dec.read_u64()?;
                if npc >= NPC_COUNT as u64 {
                    return Err(SaveError::Integrity("region-director-save/npc-bound"));
                }
                dec.read_u64()?; // event seq
                dec.read_tick()?;
                dec.read_u64()?; // cursor
                let x = dec.read_u8()?;
                let y = dec.read_u8()?;
                if (x, y) != POSITION_DISCARDED && (x >= REGION_GRID || y >= REGION_GRID) {
                    return Err(SaveError::Integrity("region-director-save/position-bounds"));
                }
                read_flag(&mut dec, SECTION)?;
            }
        }
        match dec.read_u8()? {
            0..=2 => {}
            discriminant => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(discriminant),
                });
            }
        }
        dec.read_u64()?; // reconstructed_activities
        dec.read_u64()?; // last_dormancy_span
        region_index += 1;
    }
    // The capture ledger: the in-band schema version, the two maps
    // in canonical strict-ascending order, and (v4) the compacted
    // lane summary.
    let lane_compacted = match dec.read_u8()? {
        3 => false,
        4 => true,
        discriminant => {
            return Err(SaveError::Enum {
                section: SECTION,
                discriminant: u64::from(discriminant),
            });
        }
    };
    let mut ledger_ids: Vec<ConsequenceId> = Vec::new();
    let pending_count = dec.read_count(NPC_COUNT as u64, SECTION, "pending-count")?;
    let mut prev: Option<ConsequenceId> = None;
    for _ in 0..pending_count {
        let id = read_consequence_id(&mut dec)?;
        dec.read_u64()?; // delta
        if let Some(prior) = prev
            && id <= prior
        {
            return Err(SaveError::Order { section: SECTION });
        }
        prev = Some(id);
        ledger_ids.push(id);
    }
    let applied_count = dec.read_count(MAX_COLLECTION_COUNT, SECTION, "applied-count")?;
    let mut prev: Option<ConsequenceId> = None;
    for _ in 0..applied_count {
        let id = read_consequence_id(&mut dec)?;
        dec.read_u64()?; // amount
        if let Some(prior) = prev
            && id <= prior
        {
            return Err(SaveError::Order { section: SECTION });
        }
        prev = Some(id);
        ledger_ids.push(id);
    }
    let boundary = if lane_compacted {
        let mut boundary = [0u64; REGION_COUNT];
        for slot in boundary.iter_mut() {
            *slot = dec.read_u64()?;
        }
        let totals_count = dec.read_count(NPC_COUNT as u64, SECTION, "lane-totals-count")?;
        if totals_count != NPC_COUNT as u64 {
            return Err(SaveError::Integrity(
                "region-director-save/lane-totals-count",
            ));
        }
        for _ in 0..totals_count {
            dec.read_u64()?;
        }
        Some(boundary)
    } else {
        None
    };
    // THE COMPACTED LANE GATES (fail-closed, the accepted rules):
    // the retained lanes are disjoint from the summarized coverage,
    // and the boundary never exceeds a region's current episode
    // counter (no summarizing future episodes).
    if let Some(boundary) = &boundary {
        for id in &ledger_ids {
            if id.episode <= boundary[id.region as usize] {
                return Err(SaveError::Integrity("region-director-save/lane-overlap"));
            }
        }
        for region in 0..REGION_COUNT {
            if boundary[region] > episodes[region] {
                return Err(SaveError::Integrity(
                    "region-director-save/lane-boundary-frontier",
                ));
            }
        }
    }
    dec.finish()?;
    Ok(boundary)
}

/// Reads a consequence identity from the director section (the
/// shared identity-bounds rule).
fn read_consequence_id(dec: &mut Decoder) -> Result<ConsequenceId, SaveError> {
    let region = dec.read_u32()?;
    let episode = dec.read_u64()?;
    let npc = dec.read_u32()?;
    if region as usize >= REGION_COUNT || npc as usize >= NPC_COUNT {
        return Err(SaveError::Integrity("region-director-save/identity-bounds"));
    }
    Ok(ConsequenceId::new(region, episode, npc))
}

/// Reads a LOD level discriminant (fail-closed on unknown values —
/// the wire-level discriminants only; the game enum belongs to the
/// full restore).
fn read_lod_level(dec: &mut Decoder, section: &'static str) -> Result<(), SaveError> {
    match dec.read_u8()? {
        0..=3 => Ok(()),
        discriminant => Err(SaveError::Enum {
            section,
            discriminant: u64::from(discriminant),
        }),
    }
}

/// Reads a boolean flag (fail-closed on non-0/1 discriminants).
fn read_flag(dec: &mut Decoder, section: &'static str) -> Result<bool, SaveError> {
    match dec.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        discriminant => Err(SaveError::Enum {
            section,
            discriminant: u64::from(discriminant),
        }),
    }
}

/// Reads a finite f64 (the accepted finite-float rule).
fn read_finite_f64(dec: &mut Decoder) -> Result<f64, SaveError> {
    let raw = dec.read_u64()?;
    let value = f64::from_bits(raw);
    if value.is_finite() {
        Ok(value)
    } else {
        Err(SaveError::Integrity("region-director-save/finite-float"))
    }
}
