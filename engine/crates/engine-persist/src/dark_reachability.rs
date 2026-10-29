//! The S6-C4 Phase 2 K2-B DARK REACHABILITY MODEL — a test-only,
//! strictly ISOLATED chain-graph visitor for the future sweep's
//! live-set computation. **Dark by construction:** no production
//! read, write, or reclamation path depends on this module; it
//! never calls quarantine, delete, reclaim, or any mutating
//! operation — it READS records and objects through the same
//! read-only [`ObjectSource`] the dark reader uses, verifies every
//! identity it walks, and returns the live set as DATA. The future
//! sweep implementation (the K2-C checkpoint, should it be
//! authorized) will bind this model to the real mediums; nothing
//! here modifies filesystem or SQLite provider state, and
//! [`crate::CanonicalMedium::validate`] and the providers' sweep
//! behavior are untouched.
//!
//! **The model.** Given the RETAINED revision heads (the roots the
//! operator keeps), the visitor walks the revision chain
//! (parent links, founding-terminated, deduplicated, bounded) and
//! classifies every walked record's content references:
//!
//! - a **composite-root reference** — an object whose leading
//!   domain is the ratified `engine-slice/session-snapshot/2` — is
//!   expanded through the K2-B dark reader: the root's ENTIRE
//!   declared chunk closure is loaded, hash-verified, and joined
//!   to the live set (the verified read — a missing or corrupt
//!   chunk fails the walk closed);
//! - a **direct object reference** (a legacy `/1` session object,
//!   a blob, anything else) joins the live set as itself;
//! - every walked revision record's own identity joins the live
//!   set (the chain must remain walkable).
//!
//! The result distinguishes exactly what a future sweep needs: the
//! distinct reachable chunk identities (the UNION — a chunk shared
//! by several retained roots appears ONCE), the per-chunk reference
//! counts across DISTINCT reachable roots (the sharing structure),
//! the composite roots and direct objects, and — via
//! [`LiveSet::eligible_for_retirement`] — the roots NOT reachable
//! from the retained heads (a dropped head's chain): retiring a
//! root removes only ITS contribution; a chunk still required by
//! another retained root SURVIVES (the shared-chunk reclamation
//! invariant the K2-A fence design depends on).
//!
//! The memo ([`VerificationMemo`]) is threaded through the whole
//! walk: chunks shared across the visited roots are read and
//! hash-verified ONCE per walk — the bounded verification operation
//! the memo is defined over.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::Decoder;
use engine_core::wire::MAX_TAG_LEN;

use crate::StoredObject;
use crate::composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN;
use crate::composite_reader::{
    CompositeReadError, ObjectSource, ReadLimits, VerificationMemo,
    read_composite_closure_with_memo,
};
use crate::hash::{Hash, hash_object};
use crate::record::RevisionRecord;

/// The walk's resource limits — the revision-chain and
/// composite-root bounds. A walk that exceeds either fails typed;
/// no unbounded traversal exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReachabilityLimits {
    /// The maximum distinct revision records walked (the chain
    /// bound — parents and retained heads together).
    pub max_revisions: u64,
    /// The maximum distinct composite roots expanded.
    pub max_roots: u64,
}

impl Default for ReachabilityLimits {
    fn default() -> Self {
        ReachabilityLimits {
            max_revisions: 1 << 16,
            max_roots: 1 << 16,
        }
    }
}

/// The verified live set of one reachability walk — the DATA a
/// future sweep would consume (never executed here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSet {
    /// The retained heads the walk started from.
    pub retained_heads: Vec<Hash>,
    /// Every revision record walked (the retained heads and all
    /// their ancestors — the chain that must remain walkable).
    pub visited_revisions: BTreeSet<Hash>,
    /// The reachable composite roots (distinct).
    pub composite_roots: BTreeSet<Hash>,
    /// The distinct reachable chunk identities — the UNION over
    /// every expanded root's verified closure.
    pub chunk_identities: BTreeSet<Hash>,
    /// The direct (non-composite) reachable content objects.
    pub direct_objects: BTreeSet<Hash>,
    /// Per chunk: how many DISTINCT reachable composite roots
    /// reference it (the sharing structure — a chunk shared by two
    /// retained roots counts 2 and survives either root's
    /// retirement).
    pub chunk_reference_counts: BTreeMap<Hash, u64>,
}

impl LiveSet {
    /// The roots (from the caller's known universe) NOT reachable
    /// from the retained heads — eligible for retirement. A retired
    /// root's exclusively-owned chunks leave the live set with it;
    /// a chunk another RETAINED root still requires does NOT.
    pub fn eligible_for_retirement(&self, known_roots: &[Hash]) -> Vec<Hash> {
        known_roots
            .iter()
            .filter(|root| !self.composite_roots.contains(root))
            .copied()
            .collect()
    }
}

/// Every walk failure is typed and identity-preserving; the walk
/// never guesses and never partially succeeds.
#[derive(Debug, Clone, PartialEq)]
pub enum ReachError {
    /// A record or object the chain requires is absent.
    Missing { identity: Hash },
    /// An object's bytes do not hash to the walked identity.
    HashMismatch { identity: Hash },
    /// A walked object does not decode as a revision record (the
    /// chain is malformed at this identity).
    MalformedRecord { identity: Hash },
    /// A referenced object's leading bytes do not decode as a
    /// domain-tagged object at all (neither expandable nor a
    /// well-formed direct object).
    MalformedObject { identity: Hash },
    /// The source failed for a walked identity without proving
    /// absence — the walk fails closed, concluding nothing.
    Unavailable { identity: Hash, detail: String },
    /// A composite root's closure failed the verified read.
    Composite {
        root: Hash,
        error: Box<CompositeReadError>,
    },
    /// A resource limit refused the walk.
    Limit {
        kind: &'static str,
        value: u64,
        max: u64,
    },
}

impl std::fmt::Display for ReachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReachError::Missing { identity } => {
                write!(f, "the chain requires {} which is missing", identity.hex())
            }
            ReachError::HashMismatch { identity } => {
                write!(
                    f,
                    "the object {} does not hash to its identity",
                    identity.hex()
                )
            }
            ReachError::MalformedRecord { identity } => {
                write!(f, "the revision {} does not decode", identity.hex())
            }
            ReachError::MalformedObject { identity } => {
                write!(
                    f,
                    "the object {} carries no decidable domain",
                    identity.hex()
                )
            }
            ReachError::Unavailable { identity, detail } => {
                write!(
                    f,
                    "the source failed for {} without proving absence: {detail}",
                    identity.hex()
                )
            }
            ReachError::Composite { root, error } => {
                write!(f, "the composite root {} failed: {error}", root.hex())
            }
            ReachError::Limit { kind, value, max } => {
                write!(f, "the walk limit {kind} refuses {value} (max {max})")
            }
        }
    }
}

impl std::error::Error for ReachError {}

/// Walks the retained heads' revision chain and computes the
/// VERIFIED live set — every record, every direct object, and every
/// composite root's fully hash-verified chunk closure — with the
/// shared verification memo threading the whole walk. Read-only:
/// the walk reads through the source and never mutates anything.
pub fn live_set(
    source: &mut dyn ObjectSource,
    retained_heads: &[Hash],
    limits: &ReachabilityLimits,
    memo: &mut VerificationMemo,
) -> Result<LiveSet, ReachError> {
    let mut visited_revisions = BTreeSet::new();
    let mut composite_roots = BTreeSet::new();
    let mut chunk_identities = BTreeSet::new();
    let mut direct_objects = BTreeSet::new();
    let mut chunk_reference_counts = BTreeMap::new();
    // The worklist of record identities (retained heads first; the
    // dedup set makes diamonds and shared tails walk once).
    let mut pending: Vec<Hash> = retained_heads.to_vec();
    while let Some(record_id) = pending.pop() {
        if visited_revisions.contains(&record_id) {
            continue;
        }
        if visited_revisions.len() as u64 >= limits.max_revisions {
            return Err(ReachError::Limit {
                kind: "reachability/revision-count",
                value: visited_revisions.len() as u64 + 1,
                max: limits.max_revisions,
            });
        }
        let bytes = read_verified(source, &record_id)?;
        let record = RevisionRecord::decode(&bytes).map_err(|_| ReachError::MalformedRecord {
            identity: record_id,
        })?;
        visited_revisions.insert(record_id);
        for reference in &record.content_refs {
            let bytes = read_verified(source, reference)?;
            // The classification: the leading domain tag decides —
            // the ratified composite root expands; anything else is
            // a direct object. An object whose leading bytes do not
            // decode as a domain tag at all is malformed (the
            // store's objects are domain-framed by construction).
            let observed_domain = Decoder::new(&bytes)
                .read_str(MAX_TAG_LEN, "k2b-reachability")
                .map_err(|_| ReachError::MalformedObject {
                    identity: *reference,
                })?;
            if observed_domain == SESSION_SNAPSHOT_V2_DOMAIN {
                if composite_roots.contains(reference) {
                    continue;
                }
                if composite_roots.len() as u64 >= limits.max_roots {
                    return Err(ReachError::Limit {
                        kind: "reachability/root-count",
                        value: composite_roots.len() as u64 + 1,
                        max: limits.max_roots,
                    });
                }
                let root = StoredObject::new(bytes).map_err(|_| ReachError::MalformedObject {
                    identity: *reference,
                })?;
                let verified =
                    read_composite_closure_with_memo(source, &root, &ReadLimits::default(), memo)
                        .map_err(|error| ReachError::Composite {
                        root: *reference,
                        error: Box::new(error),
                    })?;
                composite_roots.insert(*reference);
                for identity in verified.dependency_identities() {
                    *chunk_reference_counts.entry(identity).or_insert(0) += 1;
                    chunk_identities.insert(identity);
                }
            } else {
                direct_objects.insert(*reference);
            }
        }
        if let Some(parent) = record.parent {
            pending.push(parent);
        }
    }
    Ok(LiveSet {
        retained_heads: retained_heads.to_vec(),
        visited_revisions,
        composite_roots,
        chunk_identities,
        direct_objects,
        chunk_reference_counts,
    })
}

/// Reads one object through the source and verifies the bytes hash
/// to the requested identity — the walk's own verification,
/// independent of whatever the source checked.
fn read_verified(source: &mut dyn ObjectSource, identity: &Hash) -> Result<Vec<u8>, ReachError> {
    let bytes = source.read_object(identity).map_err(|error| match error {
        crate::composite_reader::ObjectSourceError::Missing(identity) => {
            ReachError::Missing { identity }
        }
        crate::composite_reader::ObjectSourceError::Unavailable(detail) => {
            ReachError::Unavailable {
                identity: *identity,
                detail,
            }
        }
    })?;
    if hash_object(&bytes) != *identity {
        return Err(ReachError::HashMismatch {
            identity: *identity,
        });
    }
    Ok(bytes)
}
