//! (V2X-CP3-A2) THE AUTHENTICATED HISTORICAL ANCESTRY WALKER —
//! the read-only reconstruction of a checkpoint range's ORIGINAL
//! parent-by-parent history, built for correct partial-batch
//! reclamation reasoning when earlier retirement batches have
//! completed and some original revision records are physically
//! absent.
//!
//! **The evidence base (all accepted, all reused):** the CP2
//! plan-bound begin authority; the checkpoint metadata (the
//! complete retirement index — every member's identity, epoch, and
//! key); the V2 plan (the authenticated batch partition); the A0
//! ancestry witness (every indexed member's complete canonical
//! preimage, including its ORIGINAL parent link); and the A1-3
//! public verifier (the installed-witness completeness and
//! binding authority). Nothing is re-derived: the walker calls the
//! accepted machinery and consumes its authenticated outputs.
//!
//! **The traversal discipline:** the walk follows the ORIGINAL
//! parent edges — for an indexed retirement member, the
//! authenticated parent link from its WITNESS PREIMAGE (the member
//! may be physically absent; its bytes are never read as a live
//! authoritative revision); for a retained anchor (an unindexed
//! record on the original ancestry), its live canonical record.
//! The walk NEVER uses legacy checkpoint skip tables: a skip that
//! authenticates a retained target may still jump over
//! still-live anchors or future-batch members, so the historical
//! traversal refuses every shortcut and follows the original
//! edges parent-by-parent. The ancestry is validated against the
//! index-derived canonical order in lockstep (the members must
//! arrive in the canonical order; the anchors interleave between
//! them) — a contradictory ancestry refuses typed.
//!
//! **Physical presence versus logical retirement (the mandatory
//! distinction):** the walker derives every historical fact from
//! the authenticated evidence, never from physical bytes at a
//! hash. A deleted member whose hash-identical bytes are later
//! reintroduced as an ordinary content object is NOT resurrected:
//! the member's classification comes from the index and its
//! original facts from the preimage, and the walk never consults
//! the member's physical presence. Conversely, a live anchor's
//! content references are reported as the anchor's own facts,
//! fully distinguishable from any historical retirement status
//! that coincides with their hashes.
//!
//! **The completion-receipt limitation (explicit):** the walker
//! classifies each member's plan batch and its position relative
//! to the caller's selected evaluation batch — `PriorBatch`,
//! `CurrentBatch`, `FutureBatch`. It does NOT claim that a
//! prior-batch member was legitimately completed: the historical
//! completion-marker receipt trust policy remains an unratified
//! owner decision, and A2 does not elevate it. The prior-batch
//! classification is a plan-structure fact (the member's batch is
//! earlier than the selected evaluation), never a completion
//! proof, never a deletion permit.
//!
//! **The A2/A3 boundary:** A2 reconstructs and classifies the
//! authenticated historical ancestry. A3 — separately authorized —
//! will consume these steps for the exact preservation accounting
//! (the Mandatory/Deferred sets, the shared-object distinctions,
//! the deletion partitions). A2 emits no deletion list, no
//! completion receipt, no durability certification.
//!
//! **The resource contract:** the walk is a bounded streaming
//! traversal — one step at a time through the caller's callback;
//! the witness shards are consumed one at a time in the canonical
//! order (the ancestry order equals the canonical order, so the
//! shard stream is sequential — no aggregate preimage retention,
//! no per-shard full-chain discovery (the single multi-key family
//! discovery is reused), no repeated aggregate witness
//! verification); the retained state is O(index size) identities
//! and the current shard's entries. The traversal bound is the
//! established policy class (2^20), with the exhaustion refusing
//! typed — never an incomplete traversal reported as complete.

use crate::checkpoint::ReclaimedIndexEntry;
use crate::hash::{Hash, hash_object};
use crate::medium::{CanonicalMedium, MediumError, StoredObject};
use crate::record::RevisionRecord;

/// The traversal bound — the same safety-valve policy class as
/// every accepted walk.
const HISTORY_WALK_BOUND: u64 = 1 << 20;

/// A member's position relative to the caller's selected
/// evaluation batch. A plan-structure fact — never a completion
/// proof (see the module documentation's completion-receipt
/// limitation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchPosition {
    /// The member's plan batch is earlier than the selected
    /// evaluation batch. NOT a completion claim.
    PriorBatch,
    /// The member belongs to the selected evaluation batch.
    CurrentBatch,
    /// The member's plan batch is later than the selected
    /// evaluation batch — LIVE for the current evaluation's safety
    /// analysis.
    FutureBatch,
}

/// One authenticated historical ancestry step, streamed to the
/// caller's callback in original descent order (newest first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoricalStep {
    /// An indexed retirement member: the authenticated ORIGINAL
    /// facts from its witness preimage (which may be the only
    /// surviving evidence — the member's record may be physically
    /// absent), with its plan batch and position.
    Member {
        /// The member's indexed identity (== the preimage's
        /// SHA-256).
        identity: Hash,
        /// The member's original chain epoch.
        epoch: u64,
        /// The member's original idempotency key, where present.
        idempotency_key: Option<Vec<u8>>,
        /// The member's plan batch index.
        batch: u64,
        /// The batch's position relative to the selected
        /// evaluation.
        position: BatchPosition,
        /// The member's ORIGINAL parent identity (the next
        /// historical record).
        parent: Hash,
        /// The member's original content references (the shared
        /// object accounting facts for the later A3).
        content_refs: Vec<Hash>,
    },
    /// A retained anchor: an unindexed record on the original
    /// ancestry (never a retirement member — its liveness is the
    /// index-derived classification, not a physical-read
    /// inference). Its facts come from its live canonical record.
    Anchor {
        /// The anchor's identity.
        identity: Hash,
        /// The anchor's chain epoch.
        epoch: u64,
        /// The anchor's parent identity (the next historical
        /// record).
        parent: Hash,
        /// The anchor's content references.
        content_refs: Vec<Hash>,
    },
    /// The traversal boundary: the prior checkpoint's MK (its
    /// first content object carries the checkpoint-metadata
    /// domain) or the campaign founding.
    Boundary {
        /// The boundary record's identity.
        identity: Hash,
        /// Whether the boundary is a prior checkpoint MK (true) or
        /// the founding (false).
        prior_checkpoint: bool,
    },
}

/// The completed traversal's summary. `Ok` always means the
/// requested traversal REACHED its boundary — every failure is a
/// typed `Err`, never a silently incomplete walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryWalkSummary {
    /// The number of member steps emitted.
    pub members: u64,
    /// The number of anchor steps emitted.
    pub anchors: u64,
    /// The boundary record's identity.
    pub boundary: Hash,
    /// Whether the boundary is a prior checkpoint MK.
    pub prior_checkpoint: bool,
}

/// (V2X-CP3-A2) THE AUTHENTICATED HISTORICAL ANCESTRY WALK — the
/// read-only, bounded, streaming reconstruction of one checkpoint
/// range's original parent-by-parent history, from the range's SK
/// (the self-retiring top member) down to the traversal boundary
/// (the prior checkpoint's MK or the campaign founding).
///
/// The prerequisites are established FIRST and completely: the
/// A1-3 installed-witness verification must return `Verified` (the
/// witness complete and bound to the MK, the plan revision, the
/// CP2 begin revision, and the metadata object); the CP2 begin
/// authority is re-established (the freshness discipline — a
/// previously returned `Verified` is not a permanent storage
/// guarantee, and every consumed preimage is re-authenticated at
/// consumption through its hash-verified read); the plan and the
/// checkpoint metadata are re-derived through the accepted
/// discoveries. The `current_batch` selects the evaluation
/// perspective for the member classifications and must be one of
/// the plan's batches.
///
/// The caller's callback receives each step; a callback error
/// propagates as the walk's error. The walk performs NO mutation
/// of any kind: no appends, no reclamation, no journal changes, no
/// marker publication — it is a read-only traversal over the
/// authenticated evidence.
pub fn walk_authenticated_history(
    medium: &mut dyn CanonicalMedium,
    mk: &Hash,
    current_batch: u64,
    on_step: &mut dyn FnMut(&HistoricalStep) -> Result<(), MediumError>,
) -> Result<HistoryWalkSummary, MediumError> {
    walk_authenticated_history_bounded(medium, mk, current_batch, on_step, HISTORY_WALK_BOUND)
}

/// The traversal-bound-parameterized form (the RC2 discipline: the
/// production entry fixes the ratified bound; the regressions
/// drive a small bound).
pub fn walk_authenticated_history_bounded(
    medium: &mut dyn CanonicalMedium,
    mk: &Hash,
    current_batch: u64,
    on_step: &mut dyn FnMut(&HistoricalStep) -> Result<(), MediumError>,
    bound: u64,
) -> Result<HistoryWalkSummary, MediumError> {
    // ===== Phase 0 — the installed-witness completeness (the
    // accepted A1-3 verifier, reused verbatim — no weakening). =====
    let manifest = match crate::ancestry_witness::verify_installed_ancestry_witness(medium, mk)? {
        crate::ancestry_witness::InstalledWitnessVerification::Verified { manifest, .. } => {
            manifest
        }
        _ => {
            return Err(MediumError::Chain {
                rule: "history/witness-not-verified",
            });
        }
    };
    // ===== Phase 1 — the authority-side reestablishment (the
    // freshness discipline: the artifacts the walk consumes are
    // re-derived NOW, and every preimage is re-authenticated at
    // its consumption below). =====
    medium.verify_begin_authority_v2(mk)?;
    let Some((_plan_record, plan)) = crate::reclaim::find_installed_plan(medium, mk)? else {
        return Err(MediumError::Chain {
            rule: "history/plan-absent",
        });
    };
    let mk_record = medium.read_record(mk)?;
    if mk_record.content_refs.len() != 1 {
        return Err(MediumError::Chain {
            rule: "history/mk-metadata-binding",
        });
    }
    let meta_bytes = medium.read_object(&mk_record.content_refs[0])?;
    let metadata =
        crate::checkpoint::CheckpointMetadata::decode(&meta_bytes).map_err(MediumError::Format)?;
    crate::ancestry_witness::validate_index_shape(&metadata.index).map_err(MediumError::Format)?;
    if !plan
        .batches
        .iter()
        .any(|batch| batch.index == current_batch)
    {
        return Err(MediumError::Chain {
            rule: "history/unknown-batch",
        });
    }
    // ===== Phase 2 — the walk setup (O(index size) identities;
    // the current shard's entries at a time). =====
    let index_by_identity: std::collections::BTreeMap<Hash, &ReclaimedIndexEntry> = metadata
        .index
        .iter()
        .map(|entry| (entry.hash, entry))
        .collect();
    let batch_by_identity: std::collections::BTreeMap<Hash, u64> = plan
        .batches
        .iter()
        .flat_map(|batch| batch.records.iter().map(move |id| (*id, batch.index)))
        .collect();
    // Every indexed member must belong to exactly one plan batch
    // (the plan's partition is complete — the CP2 membership
    // proof's premise, re-derived here as the walk's own
    // coherence check).
    for entry in &metadata.index {
        if !batch_by_identity.contains_key(&entry.hash) {
            return Err(MediumError::Chain {
                rule: "history/member-not-in-plan",
            });
        }
    }
    let order = crate::ancestry_witness::canonical_member_order(&metadata.index);
    // The traversal starts at the SK — the range's top member, the
    // canonical order's first entry (the highest epoch).
    if order.first().map(|entry| entry.hash) != Some(metadata.sk) {
        return Err(MediumError::Chain {
            rule: "history/sk-not-canonical-top",
        });
    }
    // The witness family discovery (the single multi-key walk,
    // reused) — the shard objects consumed one at a time below.
    let discovery = crate::ancestry_witness::find_installed_witness_family(medium, mk)?;
    let Some((_manifest_record, _verified_manifest)) = discovery.manifest else {
        return Err(MediumError::Chain {
            rule: "history/witness-not-verified",
        });
    };
    let shard_object_ids = discovery.shards;
    let shard_count = manifest.shard_count;
    if shard_object_ids.len() as u64 != shard_count {
        return Err(MediumError::Chain {
            rule: "history/witness-not-verified",
        });
    }
    // ===== Phase 3 — the parent-by-parent traversal (NEVER a
    // legacy skip: the original edges only). =====
    let mut cursor = metadata.sk;
    let mut member_position = 0usize;
    let mut shard_cursor = 0u64;
    let mut shard_entries: Vec<crate::ancestry_witness::WitnessEntry> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut members = 0u64;
    let mut anchors = 0u64;
    for _ in 0..bound {
        if !seen.insert(cursor) {
            return Err(MediumError::Chain {
                rule: "history/cycle",
            });
        }
        if let Some(index_entry) = index_by_identity.get(&cursor) {
            // A MEMBER: the authenticated original facts from the
            // witness preimage (the physical record is NEVER
            // consulted — the member may be absent, and
            // reintroduced bytes never resurrect it).
            let expected =
                order
                    .get(member_position)
                    .map(|entry| entry.hash)
                    .ok_or(MediumError::Chain {
                        rule: "history/ancestry-order-contradiction",
                    })?;
            if expected != cursor {
                return Err(MediumError::Chain {
                    rule: "history/ancestry-order-contradiction",
                });
            }
            if shard_entries.is_empty() {
                // The next shard in the canonical order (the
                // ancestry order equals the canonical order, so
                // the stream is sequential).
                if shard_cursor >= shard_count {
                    return Err(MediumError::Chain {
                        rule: "history/member-not-in-witness",
                    });
                }
                let Some((_record, object)) = shard_object_ids[shard_cursor as usize] else {
                    return Err(MediumError::Chain {
                        rule: "history/member-not-in-witness",
                    });
                };
                let bytes = medium.read_object(&object)?;
                let shard = crate::ancestry_witness::AncestryWitnessShard::decode(&bytes)
                    .map_err(MediumError::Format)?;
                shard_entries = shard.entries;
                shard_cursor += 1;
            }
            let entry = shard_entries.remove(0);
            // The preimage re-authentication at consumption: the
            // SHA-256 equality with the indexed identity, the
            // strict canonical REGULAR decode, and the index facts.
            if hash_object(&entry.preimage) != entry.member || entry.member != cursor {
                return Err(MediumError::Chain {
                    rule: "history/member-preimage-mismatch",
                });
            }
            let record = RevisionRecord::decode(&entry.preimage).map_err(MediumError::Format)?;
            if record.epoch != index_entry.epoch || record.idempotency_key != index_entry.key {
                return Err(MediumError::Chain {
                    rule: "history/member-facts-mismatch",
                });
            }
            let batch = batch_by_identity
                .get(&cursor)
                .copied()
                .ok_or(MediumError::Chain {
                    rule: "history/member-not-in-plan",
                })?;
            let position = if batch < current_batch {
                BatchPosition::PriorBatch
            } else if batch == current_batch {
                BatchPosition::CurrentBatch
            } else {
                BatchPosition::FutureBatch
            };
            let Some(parent) = record.parent else {
                return Err(MediumError::Chain {
                    rule: "history/member-parent-missing",
                });
            };
            on_step(&HistoricalStep::Member {
                identity: cursor,
                epoch: record.epoch,
                idempotency_key: record.idempotency_key.clone(),
                batch,
                position,
                parent,
                content_refs: record.content_refs.clone(),
            })?;
            members += 1;
            member_position += 1;
            cursor = parent;
        } else {
            // An anchor or the boundary: the LIVE canonical record
            // (an unindexed record on the original ancestry is
            // never a retirement member — absent ones refuse
            // closed below).
            let record = medium.read_record(&cursor)?;
            let first_domain = match record.content_refs.first() {
                Some(object) => {
                    let bytes = medium.read_object(object)?;
                    crate::checkpoint::peek_object_domain(&bytes)
                        .map_err(MediumError::Format)?
                        .to_owned()
                }
                None => String::new(),
            };
            if first_domain == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN {
                // The prior checkpoint's MK — the permanent
                // boundary.
                on_step(&HistoricalStep::Boundary {
                    identity: cursor,
                    prior_checkpoint: true,
                })?;
                return Ok(HistoryWalkSummary {
                    members,
                    anchors,
                    boundary: cursor,
                    prior_checkpoint: true,
                });
            }
            let Some(parent) = record.parent else {
                // The campaign founding — the root boundary.
                on_step(&HistoricalStep::Boundary {
                    identity: cursor,
                    prior_checkpoint: false,
                })?;
                return Ok(HistoryWalkSummary {
                    members,
                    anchors,
                    boundary: cursor,
                    prior_checkpoint: false,
                });
            };
            on_step(&HistoricalStep::Anchor {
                identity: cursor,
                epoch: record.epoch,
                parent,
                content_refs: record.content_refs.clone(),
            })?;
            anchors += 1;
            cursor = parent;
        }
    }
    Err(MediumError::Chain {
        rule: "history/descent-bound",
    })
}

/// The retained-evidence helper for the A2 consumers: whether an
/// object identity is referenced by any step's content references
/// (the shared-object accounting fact — the historical retirement
/// status is the step's classification, fully distinguishable).
pub fn references_include(references: &[Hash], identity: &Hash) -> bool {
    references.iter().any(|hash| hash == identity)
}

/// A convenience: the stored-object form of a member's original
/// preimage (the authenticated bytes — the A2 fixtures' shared
/// evidence).
pub fn member_preimage_object(entry: &crate::ancestry_witness::WitnessEntry) -> StoredObject {
    StoredObject::new(entry.preimage.clone()).expect("the authenticated preimage is storable")
}

// -------------------------------------------------------------
// (V2X-CP3-A2) THE HISTORICAL-ANCESTRY WALKER REGRESSIONS.
// -------------------------------------------------------------

#[cfg(all(test, feature = "composite-test-fixture"))]
mod a2_tests {
    use super::*;
    use crate::medium::CanonicalMedium;

    fn temp_dir_path(prefix: &str, label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "s7-a2-{prefix}-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn a2_store(label: &str) -> (std::path::PathBuf, crate::LocalFileMedium) {
        let root = temp_dir_path("store", label);
        std::fs::create_dir_all(&root).unwrap();
        (root.clone(), crate::LocalFileMedium::open(root).unwrap())
    }

    fn a2_journal(label: &str) -> std::path::PathBuf {
        temp_dir_path("journal", label)
    }

    fn a2_members(label: &str) -> Vec<(usize, Vec<u8>)> {
        vec![
            (48, format!("a2-{label}-payload-m0").into_bytes()),
            (48, format!("a2-{label}-payload-m1").into_bytes()),
        ]
    }

    /// The store's complete file inventory (the names and sizes of
    /// every file under the root) — the read-only proof's basis.
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

    /// Collects the walk's steps.
    fn collect_steps(
        medium: &mut dyn CanonicalMedium,
        mk: &Hash,
        current_batch: u64,
    ) -> (HistoryWalkSummary, Vec<HistoricalStep>) {
        let mut steps = Vec::new();
        let summary = walk_authenticated_history(medium, mk, current_batch, &mut |step| {
            steps.push(step.clone());
            Ok(())
        })
        .unwrap();
        (summary, steps)
    }

    /// (A2-T1) The complete original history: a fully intact
    /// checkpoint range — the walker reproduces the original
    /// authenticated ancestry with the correct retained-anchor
    /// positions, from the SK through every member and anchor to
    /// the founding boundary.
    #[cfg(unix)]
    #[test]
    fn the_complete_history_walks_parent_by_parent() {
        let (root, mut medium) = a2_store("t1");
        let journal = a2_journal("t1");
        let members = a2_members("t1");
        let campaign = crate::ancestry_witness::a12_tests::a12_campaign_with_limit(
            &mut medium,
            "t1",
            &journal,
            &members,
            10_000,
        );
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        let (summary, steps) = collect_steps(&mut medium, &campaign.mk, 0);
        // The complete descent: SK, m1, m0 (the three indexed
        // members, the canonical order), then the retained anchors
        // p1 and the journal binding, then the founding boundary.
        assert_eq!(summary.members, 3);
        assert_eq!(summary.anchors, 2);
        assert!(!summary.prior_checkpoint);
        let identities: Vec<Hash> = steps.iter().map(step_identity).collect();
        assert_eq!(identities[0], campaign.sk);
        assert_eq!(identities[1], campaign.members[1]);
        assert_eq!(identities[2], campaign.members[0]);
        // The members' original parent edges (the authenticated
        // witness preimages — the records are intact here, but the
        // walk never consults them for the members' facts).
        assert_eq!(step_parent(&steps[0]), campaign.members[1]);
        assert_eq!(step_parent(&steps[1]), campaign.members[0]);
        // The anchors between the members and the boundary.
        assert!(matches!(steps[3], HistoricalStep::Anchor { .. }));
        assert!(matches!(steps[4], HistoricalStep::Anchor { .. }));
        assert_eq!(step_parent(&steps[3]), identities_of(&steps)[4]);
        // The founding boundary: the last anchor's parent.
        assert!(matches!(steps[5], HistoricalStep::Boundary { .. }));
        assert_eq!(summary.boundary, step_parent(&steps[4]));
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    fn step_identity(step: &HistoricalStep) -> Hash {
        match step {
            HistoricalStep::Member { identity, .. }
            | HistoricalStep::Anchor { identity, .. }
            | HistoricalStep::Boundary { identity, .. } => *identity,
        }
    }

    fn step_parent(step: &HistoricalStep) -> Hash {
        match step {
            HistoricalStep::Member { parent, .. } | HistoricalStep::Anchor { parent, .. } => {
                *parent
            }
            HistoricalStep::Boundary { .. } => Hash::ZERO,
        }
    }

    fn identities_of(steps: &[HistoricalStep]) -> Vec<Hash> {
        steps.iter().map(step_identity).collect()
    }

    /// (A2-T2) The interleaved anchors and multiple batches: the
    /// plan partitioned into multiple batches (the shrunken ledger
    /// limit), the members classified Prior/Current/Future relative
    /// to the selected evaluation batch, and the retained anchors
    /// between them accounted for — the flattened plan list alone
    /// demonstrably carries no anchor positions.
    #[cfg(unix)]
    #[test]
    fn the_interleaved_batches_and_anchors_are_accounted() {
        let (root, mut medium) = a2_store("t2");
        let journal = a2_journal("t2");
        let members = a2_members("t2");
        let campaign = crate::ancestry_witness::a12_tests::a12_campaign_with_limit(
            &mut medium,
            "t2",
            &journal,
            &members,
            500,
        );
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        let (_plan_record, plan) = crate::reclaim::find_installed_plan(&mut medium, &campaign.mk)
            .unwrap()
            .unwrap();
        assert!(
            plan.batches.len() >= 2,
            "the shrunken ledger limit partitions the plan: {:?}",
            plan.batches.len()
        );
        let selected = plan.batches[plan.batches.len() - 1].index;
        let (summary, steps) = collect_steps(&mut medium, &campaign.mk, selected);
        assert_eq!(summary.members, 3);
        assert_eq!(summary.anchors, 2);
        // Every member carries its plan batch and the position
        // relative to the selected evaluation; the member batches
        // agree with the plan's partition.
        for step in &steps {
            if let HistoricalStep::Member {
                identity,
                batch,
                position,
                ..
            } = step
            {
                let expected = plan
                    .batches
                    .iter()
                    .find(|b| b.records.contains(identity))
                    .unwrap()
                    .index;
                assert_eq!(*batch, expected);
                let expected_position = if expected < selected {
                    BatchPosition::PriorBatch
                } else if expected == selected {
                    BatchPosition::CurrentBatch
                } else {
                    BatchPosition::FutureBatch
                };
                assert_eq!(*position, expected_position);
            }
        }
        // The flattened plan list alone cannot produce the anchor
        // steps: the walk's anchors are NOT plan members.
        let plan_records: Vec<Hash> = plan
            .batches
            .iter()
            .flat_map(|batch| batch.records.iter().copied())
            .collect();
        for step in &steps {
            if let HistoricalStep::Anchor { identity, .. } = step {
                assert!(
                    !plan_records.contains(identity),
                    "the anchor is not a plan member — the flattened list alone misses it"
                );
            }
        }
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (A2-T3) The physically missing completed members: a
    /// legitimate test-only partial retirement (the prior-batch
    /// members' record files directly removed — no production
    /// deletion path invoked) with the authenticated witness
    /// preimages intact — the walk reconstructs the ORIGINAL
    /// parent relationships without reading the missing records as
    /// live authoritative revisions.
    #[cfg(unix)]
    #[test]
    fn the_missing_members_reconstruct_from_the_witness() {
        let (root, mut medium) = a2_store("t3");
        let journal = a2_journal("t3");
        let members = a2_members("t3");
        let campaign = crate::ancestry_witness::a12_tests::a12_campaign_with_limit(
            &mut medium,
            "t3",
            &journal,
            &members,
            500,
        );
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        let (summary_before, steps_before) = collect_steps(&mut medium, &campaign.mk, 0);
        // The test-only partial retirement: the prior-batch
        // members' RECORD objects removed (the witness shard
        // content objects are different files and remain intact).
        let (_plan_record, plan) = crate::reclaim::find_installed_plan(&mut medium, &campaign.mk)
            .unwrap()
            .unwrap();
        let prior_members: Vec<Hash> = plan
            .batches
            .iter()
            .filter(|batch| batch.index < plan.batches.len() as u64 - 1)
            .flat_map(|batch| batch.records.iter().copied())
            .collect();
        assert!(!prior_members.is_empty());
        for id in &prior_members {
            std::fs::remove_file(medium.object_path_pub(id)).unwrap();
        }
        // The reconstruction: the SAME steps as the intact walk
        // (the members' facts from the preimages — the missing
        // records never consulted).
        let (summary_after, steps_after) = collect_steps(&mut medium, &campaign.mk, 0);
        assert_eq!(summary_before, summary_after);
        assert_eq!(steps_before, steps_after);
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (A2-T4) The unsafe skip rejection: the checkpoint's own
    /// self-retirement skip jumps from the SK past EVERY range
    /// member to the retained p1 — a shortcut a skip-following
    /// walker would use to omit the current and future-batch
    /// members entirely. The historical walk NEVER uses a skip: it
    /// follows the original parent edges and visits every member
    /// and anchor the skip would bypass.
    #[cfg(unix)]
    #[test]
    fn the_unsafe_skip_is_never_used() {
        let (root, mut medium) = a2_store("t4");
        let journal = a2_journal("t4");
        let members = a2_members("t4");
        let campaign = crate::ancestry_witness::a12_tests::a12_campaign_with_limit(
            &mut medium,
            "t4",
            &journal,
            &members,
            10_000,
        );
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        // The checkpoint's self-skip: from the SK (the MK's parent,
        // retired with the range) to p1 — jumping past m1, m0, and
        // every anchor between. Read from the authenticated
        // metadata.
        let mk_record = medium.read_record(&campaign.mk).unwrap();
        let meta_bytes = medium.read_object(&mk_record.content_refs[0]).unwrap();
        let metadata = crate::checkpoint::CheckpointMetadata::decode(&meta_bytes).unwrap();
        let self_target = metadata.self_skip().unwrap().nearest_retained_ancestor;
        // The walk visits EVERY member below the SK — none omitted
        // by the skip's jump.
        let (summary, steps) = collect_steps(&mut medium, &campaign.mk, 0);
        assert_eq!(summary.members, 3);
        assert_eq!(summary.anchors, 2);
        let identities = identities_of(&steps);
        // The skip's target is reached only through the FULL
        // parent-by-parent descent — after every member.
        assert!(identities.contains(&self_target));
        let skip_target_position = identities.iter().position(|id| *id == self_target).unwrap();
        assert!(
            skip_target_position >= 3,
            "the skip target is reached only after every member step"
        );
        assert_eq!(identities[0], campaign.sk);
        assert_eq!(identities[1], campaign.members[1]);
        assert_eq!(identities[2], campaign.members[0]);
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (A2-T5 + A2-T6) The physical reintroduction and the shared
    /// reference: the bespoke campaign whose retained anchor p1
    /// carries the member m0's own canonical bytes as its content
    /// object (an independently live content reference at a hash
    /// that is also a retirement member's identity). The member's
    /// record is then deleted (the logical retirement) and its
    /// hash-identical bytes REINTRODUCED (the physical
    /// resurrection trap): the walk does not resurrect the retired
    /// member — its classification remains the indexed member with
    /// its batch, its facts from the preimage — and the live
    /// anchor's content reference remains fully distinguishable
    /// from that historical retirement status.
    #[cfg(unix)]
    #[test]
    fn the_reintroduced_bytes_do_not_resurrect_the_retired_member() {
        // THE BESPOKE CAMPAIGN: founding → binding → p1 →
        // shared_anchor(parent=p1, content=m0's PAYLOAD object —
        // the shared reference) → m0(parent=the anchor, the same
        // payload) → m1 → sk → metadata → mk → plan → audits →
        // begin → witness. The retained anchor carries an
        // independently live content reference to the SAME object
        // as the retirement member m0 (the A2-T6 shared-reference
        // scenario); m0's record is then logically retired (its
        // file removed) and its hash-identical bytes physically
        // REINTRODUCED at the same content-addressed path (the
        // A2-T5 resurrection trap).
        let (root, mut medium) = a2_store("t5");
        let journal = a2_journal("t5");
        let plain = {
            use engine_core::Encoder;
            StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"a2-t5-payload");
                enc.into_bytes()
            })
            .unwrap()
        };
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            "t5",
            0,
        )
        .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(&journal);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        drop(
            crate::reclaim::create_campaign_bound_journal(&mut medium, &journal, &founding_id)
                .unwrap(),
        );
        let (_binding, _outcome) = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        let step = |medium: &mut dyn CanonicalMedium, tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = crate::record::RevisionRecord::regular(head, epoch, tick)
                .with_content_refs(vec![plain.hash()]);
            medium
                .append(&record, std::slice::from_ref(&plain), Some(head))
                .unwrap();
            record.id()
        };
        let p1_id = step(&mut medium, 2);
        // m0's PAYLOAD object — the object the shared anchor and
        // m0 will BOTH reference (the shared content).
        let m0_payload = {
            use engine_core::Encoder;
            StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"a2-t5-m0-shared-payload");
                enc.into_bytes()
            })
            .unwrap()
        };
        // THE SHARED ANCHOR: a retained record between p1 and m0,
        // carrying the SAME payload object m0 will reference.
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let shared_anchor = crate::record::RevisionRecord::regular(head, epoch, 3)
            .with_content_refs(vec![m0_payload.hash()]);
        let shared_anchor_id = shared_anchor.id();
        medium
            .append(
                &shared_anchor,
                std::slice::from_ref(&m0_payload),
                Some(head),
            )
            .unwrap();
        // m0 on top of the shared anchor, referencing the same
        // payload.
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let m0 = crate::record::RevisionRecord::regular(head, epoch, 3)
            .with_idempotency_key(b"a2-t5-m0-key".to_vec())
            .with_content_refs(vec![m0_payload.hash()]);
        let m0_id = m0.id();
        medium
            .append(&m0, std::slice::from_ref(&m0_payload), Some(head))
            .unwrap();
        // m1 and SK.
        let m1_payload = {
            use engine_core::Encoder;
            StoredObject::new({
                let mut enc = Encoder::new();
                enc.write_str("sektor7/save/1");
                enc.write_bytes(b"a2-t5-m1-payload");
                enc.into_bytes()
            })
            .unwrap()
        };
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let m1 = crate::record::RevisionRecord::regular(head, epoch, 3)
            .with_idempotency_key(b"a2-t5-m1-key".to_vec())
            .with_content_refs(vec![m1_payload.hash()]);
        medium
            .append(&m1, std::slice::from_ref(&m1_payload), Some(head))
            .unwrap();
        let sk_id = step(&mut medium, 4);
        // The metadata over [m0, m1, sk] with the self-skip to p1
        // (the jump past the shared anchor and the members that
        // this walk must ignore).
        let seed = crate::checkpoint::compute_digest_seed(&founding_id);
        let mut digest = seed;
        let mut index = Vec::new();
        for id in [&m0_id, &m1.id()] {
            let record = medium.read_record(id).unwrap();
            digest = crate::checkpoint::chain_range_digest_step(&digest, &record.canonical_bytes());
            index.push(crate::checkpoint::ReclaimedIndexEntry::new(
                record.idempotency_key.clone(),
                *id,
                record.epoch,
            ));
        }
        let sk_record = medium.read_record(&sk_id).unwrap();
        index.push(crate::checkpoint::ReclaimedIndexEntry::new(
            sk_record.idempotency_key.clone(),
            sk_id,
            sk_record.epoch,
        ));
        let metadata = crate::checkpoint::CheckpointMetadata::new(
            sk_id,
            m0_id,
            vec![crate::checkpoint::SkipEntry::new(Hash::ZERO, p1_id)],
            index,
            digest,
            seed,
        );
        let metadata_object = StoredObject::new(metadata.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk = crate::record::RevisionRecord::regular(head, epoch, 5)
            .with_content_refs(vec![metadata_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&metadata_object), Some(head))
            .unwrap();
        let mk_id = mk.id();
        crate::reclaim::publish_reclamation_plan(&mut medium, &mk_id, 10_000, &journal, 6).unwrap();
        let (_plan_id, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk_id)
            .unwrap()
            .unwrap();
        for batch in &plan.batches {
            crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk_id, batch.index, 7)
                .unwrap();
        }
        medium.begin_reclamation_v2(&mk_id, 8).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(&mut medium, &mk_id, 9).unwrap();
        // The intact walk: the shared anchor visited between m0
        // and p1, and BOTH the anchor and the member reference the
        // shared payload (the A3 protection facts preserved, the
        // retirement status fully distinguishable).
        let (summary, steps) = collect_steps(&mut medium, &mk_id, 0);
        assert_eq!(summary.members, 3);
        assert_eq!(summary.anchors, 3); // the shared anchor, p1, and the journal binding
        let identities = identities_of(&steps);
        assert_eq!(identities[0], sk_id);
        assert_eq!(identities[1], m1.id());
        assert_eq!(identities[2], m0_id);
        assert_eq!(identities[3], shared_anchor_id);
        assert_eq!(identities[4], p1_id);
        assert!(matches!(steps[6], HistoricalStep::Boundary { .. }));
        match &steps[3] {
            HistoricalStep::Anchor { content_refs, .. } => {
                assert!(
                    references_include(content_refs, &m0_payload.hash()),
                    "the live anchor's shared content reference is preserved"
                );
            }
            other => panic!("the shared anchor walks as an anchor: {other:?}"),
        }
        match &steps[2] {
            HistoricalStep::Member { content_refs, .. } => {
                assert!(
                    references_include(content_refs, &m0_payload.hash()),
                    "the member's own shared reference is preserved — the facts A3 needs"
                );
            }
            other => panic!("m0 walks as a member: {other:?}"),
        }
        // THE LOGICAL RETIREMENT + THE PHYSICAL REINTRODUCTION:
        // m0's RECORD object removed (the retirement), then its
        // hash-identical bytes written back at the same
        // content-addressed path (the resurrection trap — the
        // bytes are present, the member is retired).
        let m0_record_bytes = medium.read_record(&m0_id).unwrap().canonical_bytes();
        std::fs::remove_file(medium.object_path_pub(&m0_id)).unwrap();
        std::fs::write(medium.object_path_pub(&m0_id), m0_record_bytes).unwrap();
        let (summary_after, steps_after) = collect_steps(&mut medium, &mk_id, 0);
        assert_eq!(summary, summary_after);
        assert_eq!(
            steps, steps_after,
            "the reintroduced bytes change nothing: the member is not resurrected"
        );
        match &steps_after[2] {
            HistoricalStep::Member { identity, .. } => {
                assert_eq!(
                    *identity, m0_id,
                    "m0 remains the indexed member — never an anchor"
                );
            }
            other => panic!("the reintroduced member stays a member: {other:?}"),
        }
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (A2-T7) The malformed or incomplete evidence: the missing
    /// witness shard content (the installed-state degradation),
    /// the missing manifest content, and the physically missing
    /// RETAINED ANCHOR — every one refuses closed, never a
    /// silently incomplete traversal.
    #[cfg(unix)]
    #[test]
    fn the_malformed_evidence_refuses_closed() {
        let (root, mut medium) = a2_store("t7");
        let journal = a2_journal("t7");
        let members = a2_members("t7");
        let campaign = crate::ancestry_witness::a12_tests::a12_campaign_with_limit(
            &mut medium,
            "t7",
            &journal,
            &members,
            10_000,
        );
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let publication =
            crate::ancestry_witness::publish_ancestry_witness(&mut medium, &campaign.mk, 9)
                .unwrap();
        // (a) The witness shard content deleted: the installed-state
        // verification degrades — the walk refuses before any step.
        let (_record, shard) =
            crate::ancestry_witness::find_installed_witness_shard(&mut medium, &campaign.mk, 0)
                .unwrap()
                .unwrap();
        let shard_content = hash_object(&shard.canonical_bytes());
        std::fs::remove_file(medium.object_path_pub(&shard_content)).unwrap();
        let mut steps = Vec::new();
        let error = walk_authenticated_history(&mut medium, &campaign.mk, 0, &mut |step| {
            steps.push(step.clone());
            Ok(())
        })
        .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "history/witness-not-verified"
                }
            ),
            "the degraded witness refuses the walk: {error:?}"
        );
        assert!(steps.is_empty());
        // (b) The manifest content deleted: the same refusal.
        std::fs::write(
            medium.object_path_pub(&shard_content),
            shard.canonical_bytes(),
        )
        .unwrap();
        let manifest_content = hash_object(&publication.manifest.canonical_bytes());
        std::fs::remove_file(medium.object_path_pub(&manifest_content)).unwrap();
        let error = walk_authenticated_history(&mut medium, &campaign.mk, 0, &mut |_step| Ok(()))
            .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "history/witness-not-verified"
                }
            ),
            "the degraded manifest refuses the walk: {error:?}"
        );
        std::fs::write(
            medium.object_path_pub(&manifest_content),
            publication.manifest.canonical_bytes(),
        )
        .unwrap();
        // (c) The physically missing retained anchor: the walk
        // follows the original edges into the anchor and its
        // absence refuses closed (an anchor is never a retirement
        // member — its absence is corruption, never a legitimate
        // gap).
        let mk_record = medium.read_record(&campaign.mk).unwrap();
        let meta_bytes = medium.read_object(&mk_record.content_refs[0]).unwrap();
        let metadata = crate::checkpoint::CheckpointMetadata::decode(&meta_bytes).unwrap();
        let self_target = metadata.self_skip().unwrap().nearest_retained_ancestor;
        std::fs::remove_file(medium.object_path_pub(&self_target)).unwrap();
        let error = walk_authenticated_history(&mut medium, &campaign.mk, 0, &mut |_step| Ok(()))
            .unwrap_err();
        assert!(
            matches!(error, MediumError::MissingObject { .. }),
            "the missing retained anchor refuses closed: {error:?}"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (A2-T8) The cycles and bounds: the traversal bound's
    /// exhaustion refuses typed — never an incomplete traversal
    /// reported as complete. (The cycle guard is defense-in-depth:
    // every parent edge the walk follows is hash-authenticated —
    /// the members' from the witness preimages, the anchors' from
    /// their own hash-verified records — so a cycle is
    /// unconstructible through the authenticated evidence; the
    /// bound is the reachable exhaustion class.)
    #[cfg(unix)]
    #[test]
    fn the_descent_bound_exhaustion_refuses_typed() {
        let (root, mut medium) = a2_store("t8");
        let journal = a2_journal("t8");
        let members = a2_members("t8");
        let campaign = crate::ancestry_witness::a12_tests::a12_campaign_with_limit(
            &mut medium,
            "t8",
            &journal,
            &members,
            10_000,
        );
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        // A bound of two: the walk emits the SK and m1 steps and
        // exhausts — the typed refusal, never a summary.
        let error = walk_authenticated_history_bounded(
            &mut medium,
            &campaign.mk,
            0,
            &mut |_step| Ok(()),
            2,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "history/descent-bound"
                }
            ),
            "the exhaustion refuses typed: {error:?}"
        );
        // A generous bound: the complete traversal.
        let (summary, _steps) = collect_steps(&mut medium, &campaign.mk, 0);
        assert_eq!(summary.members, 3);
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (A2-T9) The SQLite twin: the same complete traversal on the
    /// transactional provider (the Windows-compatible arm — the
    /// transactional begin admits on every platform).
    #[test]
    fn the_sqlite_twin_walks_the_complete_history() {
        let path = temp_dir_path("sqlite", "t9");
        let journal = a2_journal("t9");
        let mut medium = crate::SqliteMedium::open(&path).unwrap();
        let members = a2_members("t9");
        let campaign = crate::ancestry_witness::a12_tests::a12_campaign_with_limit(
            &mut medium,
            "t9",
            &journal,
            &members,
            10_000,
        );
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        let (summary, steps) = collect_steps(&mut medium, &campaign.mk, 0);
        assert_eq!(summary.members, 3);
        assert_eq!(summary.anchors, 2);
        assert!(!summary.prior_checkpoint);
        let identities = identities_of(&steps);
        assert_eq!(identities[0], campaign.sk);
        assert_eq!(identities[1], campaign.members[1]);
        assert_eq!(identities[2], campaign.members[0]);
        drop(medium);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        let _ = std::fs::remove_dir_all(&journal);
    }

    /// (A2-T10) The read-only and preservation proof: the complete
    /// traversal appends nothing (the HEAD never moves), deletes
    /// nothing, mutates nothing (the store's complete file
    /// inventory — names and sizes — is byte-for-byte unchanged,
    /// including the journal root), and publishes no marker.
    #[cfg(unix)]
    #[test]
    fn the_walk_is_read_only() {
        let (root, mut medium) = a2_store("t10");
        let journal = a2_journal("t10");
        let members = a2_members("t10");
        let campaign = crate::ancestry_witness::a12_tests::a12_campaign_with_limit(
            &mut medium,
            "t10",
            &journal,
            &members,
            10_000,
        );
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        crate::ancestry_witness::publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        let head_before = medium.head().unwrap().unwrap();
        let store_before = inventory(&root);
        let journal_before = inventory(&journal);
        let (_summary, _steps) = collect_steps(&mut medium, &campaign.mk, 0);
        assert_eq!(
            medium.head().unwrap().unwrap(),
            head_before,
            "the walk never moves the HEAD"
        );
        assert_eq!(inventory(&root), store_before, "the store is untouched");
        assert_eq!(
            inventory(&journal),
            journal_before,
            "the journal is untouched"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }
}
