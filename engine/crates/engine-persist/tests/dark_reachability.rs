//! The S6-C4 Phase 2 K2-B dark reachability model's contract —
//! synthetic revision chains over isolated mock sources: the mixed
//! legacy/composite classification, the verified live-set union,
//! the shared-chunk retirement invariant (a chunk two retained
//! roots share SURVIVES either root's retirement), the historical
//! dependency closure, and the fail-closed walk negatives. The
//! visitor is read-only data for a future sweep — it never
//! quarantines, deletes, or reclaims.

use std::collections::HashMap;

use engine_persist::composite_manifest::{
    self, CompositeManifest, HistoryKind, ManifestEntry, SectionKind, TAG_BLOCKED, TAG_DIRECTOR,
    TAG_FAR, TAG_HEADER, TAG_HELD, TAG_LEDGER_META, TAG_LEDGER_RECORDS, TAG_NEAR, TAG_WORLD,
};
use engine_persist::composite_reader::{ObjectSource, ObjectSourceError, VerificationMemo};
use engine_persist::dark_reachability::{
    self, LiveSet, ReachError, ReachabilityLimits, RetentionReason, RetirementCandidate,
    RetirementEligibility,
};
use engine_persist::record::{AuthorityMode, RevisionRecord};
use engine_persist::{Hash, StoredObject};

// ------------------------------------------------------------- fixtures

struct MapSource {
    objects: HashMap<Hash, Vec<u8>>,
}

impl MapSource {
    fn new() -> Self {
        MapSource {
            objects: HashMap::new(),
        }
    }

    fn insert(&mut self, identity: Hash, bytes: Vec<u8>) {
        self.objects.insert(identity, bytes);
    }

    fn remove(&mut self, identity: &Hash) {
        self.objects.remove(identity);
    }
}

impl ObjectSource for MapSource {
    fn read_object(&mut self, identity: &Hash) -> Result<Vec<u8>, ObjectSourceError> {
        self.objects
            .get(identity)
            .cloned()
            .ok_or(ObjectSourceError::Missing(*identity))
    }
}

/// One synthetic composite (a grammatical manifest naming real
/// synthetic chunk envelopes), optionally listing one SHARED chunk
/// identity in its first sealed slot (several composites may list
/// the same identity — the same tag and payload, the same content
/// identity).
struct SyntheticComposite {
    manifest: CompositeManifest,
    root: StoredObject,
    chunk_objects: Vec<(Hash, Vec<u8>)>,
}

fn synthetic_composite(variant: u8, share_chunk: Option<Hash>) -> SyntheticComposite {
    let mut seed = variant;
    let mut entries = Vec::new();
    let mut chunk_objects = Vec::new();
    let push_singleton = |entries: &mut Vec<ManifestEntry>,
                          chunk_objects: &mut Vec<(Hash, Vec<u8>)>,
                          seed: &mut u8,
                          tag: &'static str| {
        let payload = vec![*seed; 10 + *seed as usize];
        *seed = seed.wrapping_add(1);
        let envelope = composite_manifest::encode_chunk_envelope(tag, &payload);
        entries.push(ManifestEntry {
            tag,
            identity: envelope.hash(),
            payload_length: payload.len() as u64,
            kind: SectionKind::Singleton,
        });
        chunk_objects.push((envelope.hash(), envelope.into_bytes()));
    };
    push_singleton(&mut entries, &mut chunk_objects, &mut seed, TAG_HEADER);
    push_singleton(&mut entries, &mut chunk_objects, &mut seed, TAG_NEAR);
    push_singleton(&mut entries, &mut chunk_objects, &mut seed, TAG_FAR);
    push_singleton(&mut entries, &mut chunk_objects, &mut seed, TAG_BLOCKED);
    push_singleton(&mut entries, &mut chunk_objects, &mut seed, TAG_HELD);
    push_singleton(&mut entries, &mut chunk_objects, &mut seed, TAG_LEDGER_META);
    let sealed_width = 4u64 * 31;
    for sealed in 0..2usize {
        let identity = match share_chunk.filter(|_| sealed == 0) {
            Some(shared) => shared,
            None => {
                let payload = vec![seed; sealed_width as usize];
                seed = seed.wrapping_add(1);
                let envelope =
                    composite_manifest::encode_chunk_envelope(TAG_LEDGER_RECORDS, &payload);
                let identity = envelope.hash();
                chunk_objects.push((identity, envelope.into_bytes()));
                identity
            }
        };
        entries.push(ManifestEntry {
            tag: TAG_LEDGER_RECORDS,
            identity,
            payload_length: sealed_width,
            kind: SectionKind::LedgerSealed,
        });
    }
    let tail_bytes = 2u64 * 31;
    let payload = vec![seed; tail_bytes as usize];
    let envelope = composite_manifest::encode_chunk_envelope(TAG_LEDGER_RECORDS, &payload);
    entries.push(ManifestEntry {
        tag: TAG_LEDGER_RECORDS,
        identity: envelope.hash(),
        payload_length: tail_bytes,
        kind: SectionKind::LedgerTail,
    });
    chunk_objects.push((envelope.hash(), envelope.into_bytes()));
    push_singleton(&mut entries, &mut chunk_objects, &mut seed, TAG_WORLD);
    push_singleton(&mut entries, &mut chunk_objects, &mut seed, TAG_DIRECTOR);
    let composed_length = entries.iter().map(|entry| entry.payload_length).sum();
    let manifest = CompositeManifest {
        history_kind: HistoryKind::Full,
        ledger_boundary: 4,
        composed_length,
        entries,
    };
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    SyntheticComposite {
        manifest,
        root,
        chunk_objects,
    }
}

/// The SHARED chunk's envelope bytes (the identity two composites
/// list): a sealed ledger chunk with a fixed payload.
fn shared_chunk() -> (Hash, Vec<u8>) {
    let payload = vec![77u8; 4 * 31];
    let envelope = composite_manifest::encode_chunk_envelope(TAG_LEDGER_RECORDS, &payload);
    (envelope.hash(), envelope.into_bytes())
}

fn legacy_object(seed: u8) -> (Hash, Vec<u8>) {
    // A legacy /1-style direct content object: domain-framed, opaque
    // to the walk (never expanded — classified as a direct object).
    let mut enc = engine_core::Encoder::new();
    enc.write_str("sektor7/save/1");
    enc.write_bytes(&[seed; 16]);
    let object = StoredObject::new(enc.into_bytes()).unwrap();
    (object.hash(), object.into_bytes())
}

/// One revision record over the given content references; returns
/// the record's identity and canonical bytes (the record IS an
/// object in the model's unified identity space).
fn record(refs: Vec<Hash>, parent: Option<Hash>, epoch: u64) -> (Hash, Vec<u8>) {
    let record = match parent {
        None => RevisionRecord::founding(AuthorityMode::Standalone, "k2b-reach", 0),
        Some(parent) => RevisionRecord::regular(parent, epoch.saturating_sub(1), 0),
    }
    .with_content_refs(refs);
    (record.id(), record.canonical_bytes())
}

fn install(source: &mut MapSource, composite: &SyntheticComposite) {
    // The root object TOO — the walk reads every referenced object
    // by identity, the composite roots included.
    source.insert(composite.root.hash(), composite.root.bytes().to_vec());
    for (identity, bytes) in &composite.chunk_objects {
        source.insert(*identity, bytes.clone());
    }
}

// ----------------------------------------------------------------- tests

#[test]
fn the_mixed_legacy_and_composite_chain_walks_and_verifies() {
    // The synthetic chain: a founding record referencing a LEGACY
    // /1-style object, a middle revision referencing composite root
    // A, and the retained head referencing composite root B — the
    // historical root A stays reachable through the parent walk.
    let root_a = synthetic_composite(0, None);
    let root_b = synthetic_composite(30, None);
    let (legacy, legacy_bytes) = legacy_object(1);
    let mut source = MapSource::new();
    install(&mut source, &root_a);
    install(&mut source, &root_b);
    source.insert(legacy, legacy_bytes);
    let (founding, founding_bytes) = record(vec![legacy], None, 0);
    let (middle, middle_bytes) = record(vec![root_a.root.hash()], Some(founding), 1);
    let (head, head_bytes) = record(vec![root_b.root.hash()], Some(middle), 2);
    source.insert(founding, founding_bytes);
    source.insert(middle, middle_bytes);
    source.insert(head, head_bytes);

    let mut memo = VerificationMemo::new();
    let live = dark_reachability::live_set(
        &mut source,
        &[head],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap_or_else(|error| panic!("the mixed chain walks: {error:?}"));

    assert_eq!(live.retained_heads, vec![head]);
    assert_eq!(
        live.visited_revisions,
        [founding, middle, head].into_iter().collect(),
        "the parent walk covers the whole chain"
    );
    assert_eq!(
        live.composite_roots,
        [root_a.root.hash(), root_b.root.hash()]
            .into_iter()
            .collect(),
        "both roots are reachable — the historical one through the chain"
    );
    assert_eq!(live.direct_objects, [legacy].into_iter().collect());
    // The verified live-set union: every chunk of BOTH roots, each
    // referenced by exactly one distinct root here.
    let expected_union: std::collections::BTreeSet<Hash> = root_a
        .manifest
        .entries
        .iter()
        .chain(root_b.manifest.entries.iter())
        .map(|entry| entry.identity)
        .collect();
    assert_eq!(live.chunk_identities, expected_union);
    for (chunk, count) in &live.chunk_reference_counts {
        assert_eq!(*count, 1, "no sharing in this fixture: {chunk:?}");
    }
    // The memo verified exactly the union (each chunk once).
    assert_eq!(memo.len(), expected_union.len());
    // Neither reachable root is retirement-eligible while retained.
    for root in [&root_a, &root_b] {
        assert_eq!(
            live.retirement_eligibility(RetirementCandidate(root.root.hash())),
            RetirementEligibility::Retained(RetentionReason::ReachableCompositeRoot)
        );
    }
    // The reachable legacy /1 object is NEVER retirement-eligible
    // (Corrective B's core: a live legacy root cannot be
    // misclassified by a composite-only membership check).
    assert_eq!(
        live.retirement_eligibility(RetirementCandidate(legacy)),
        RetirementEligibility::Retained(RetentionReason::DirectObject),
        "a reachable legacy /1 snapshot is never retirement-eligible"
    );
    // The walked records and the chunk dependencies are protected
    // categories too.
    assert_eq!(
        live.retirement_eligibility(RetirementCandidate(head)),
        RetirementEligibility::Retained(RetentionReason::RevisionRecord)
    );
    assert_eq!(
        live.retirement_eligibility(RetirementCandidate(root_b.manifest.entries[0].identity)),
        RetirementEligibility::Retained(RetentionReason::ChunkDependency)
    );
}

#[test]
fn the_shared_chunk_survives_one_roots_retirement() {
    // TWO INDEPENDENT chains whose composite roots SHARE one sealed
    // ledger chunk X: with both heads retained, X is live and
    // referenced by two distinct roots; retiring chain A's head
    // leaves X STILL LIVE through chain B's root — and root A
    // becomes retirement-eligible.
    let (shared, shared_bytes) = shared_chunk();
    let root_a = synthetic_composite(0, Some(shared));
    let root_b = synthetic_composite(60, Some(shared));
    let mut source = MapSource::new();
    install(&mut source, &root_a);
    install(&mut source, &root_b);
    source.insert(shared, shared_bytes);
    let (head_a, head_a_bytes) = record(vec![root_a.root.hash()], None, 0);
    let (head_b, head_b_bytes) = record(vec![root_b.root.hash()], None, 0);
    source.insert(head_a, head_a_bytes);
    source.insert(head_b, head_b_bytes);

    let mut memo = VerificationMemo::new();
    let live = dark_reachability::live_set(
        &mut source,
        &[head_a, head_b],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap();
    assert!(live.chunk_identities.contains(&shared));
    assert_eq!(live.chunk_reference_counts[&shared], 2);
    // The union counts the shared chunk ONCE (the verified
    // live-set union — the memo verified it once).
    let total_chunks: usize = root_a.manifest.entries.len() + root_b.manifest.entries.len() - 1;
    assert_eq!(live.chunk_identities.len(), total_chunks);
    assert_eq!(memo.len(), total_chunks);

    // Retire chain A: only head B retained.
    let mut memo = VerificationMemo::new();
    let surviving = dark_reachability::live_set(
        &mut source,
        &[head_b],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap();
    assert!(
        surviving.chunk_identities.contains(&shared),
        "the shared chunk X is STILL REQUIRED by the retained root B"
    );
    assert_eq!(surviving.chunk_reference_counts[&shared], 1);
    // The retired chain's root is retirement-eligible — ONLY under
    // the documented candidate-universe assumption (the caller
    // asserts composite roots); the retained one is refused with
    // its protecting category.
    assert_eq!(
        surviving.retirement_eligibility(RetirementCandidate(root_a.root.hash())),
        RetirementEligibility::Eligible,
        "the retired chain's root is retirement-eligible under the documented assumption"
    );
    assert_eq!(
        surviving.retirement_eligibility(RetirementCandidate(root_b.root.hash())),
        RetirementEligibility::Retained(RetentionReason::ReachableCompositeRoot),
        "the retained root is not retirement-eligible"
    );
    // The shared chunk itself, passed as a candidate, is protected
    // by the retained root's dependency (a chunk is never a root,
    // and the typed contract refuses it by category).
    assert_eq!(
        surviving.retirement_eligibility(RetirementCandidate(shared)),
        RetirementEligibility::Retained(RetentionReason::ChunkDependency),
        "the shared chunk still required by a retained root is never eligible"
    );
    assert!(
        !surviving.visited_revisions.contains(&head_a),
        "the retired chain's records leave the live set"
    );
}

#[test]
fn the_missing_historical_dependency_fails_the_walk_closed() {
    // The retained head's ANCESTOR references a composite root whose
    // chunk X is absent: the verified live-set walk fails closed
    // with the missing identity (the historical dependency).
    let root_a = synthetic_composite(0, None);
    let root_b = synthetic_composite(30, None);
    let mut source = MapSource::new();
    install(&mut source, &root_a);
    install(&mut source, &root_b);
    // Root A's first sealed chunk is MISSING.
    let missing = root_a.manifest.entries[6].identity;
    source.remove(&missing);
    let (middle, middle_bytes) = record(vec![root_a.root.hash()], None, 0);
    let (head, head_bytes) = record(vec![root_b.root.hash()], Some(middle), 1);
    source.insert(middle, middle_bytes);
    source.insert(head, head_bytes);

    let mut memo = VerificationMemo::new();
    let error = dark_reachability::live_set(
        &mut source,
        &[head],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap_err();
    match error {
        ReachError::Composite { root, error } => {
            assert_eq!(root, root_a.root.hash());
            assert_eq!(
                *error,
                engine_persist::composite_reader::CompositeReadError::Missing { identity: missing },
                "the historical dependency's absence reports its identity"
            );
        }
        other => panic!("the missing historical dependency fails as Composite: {other:?}"),
    }
}

#[test]
fn the_corrupt_shared_chunk_fails_both_roots_identically() {
    // A corrupted shared chunk (the bytes no longer hash to the
    // declared identity) fails the walk — the reader's own
    // re-verification, and the memo never accepts it.
    let (shared, _) = shared_chunk();
    let root_a = synthetic_composite(0, Some(shared));
    let root_b = synthetic_composite(60, Some(shared));
    let mut source = MapSource::new();
    install(&mut source, &root_a);
    install(&mut source, &root_b);
    let mut corrupted = vec![77u8; 4 * 31];
    corrupted[0] ^= 0xff;
    let mis_hashed = composite_manifest::encode_chunk_envelope(TAG_LEDGER_RECORDS, &corrupted);
    source.insert(shared, mis_hashed.into_bytes());
    let (head_a, head_a_bytes) = record(vec![root_a.root.hash()], None, 0);
    let (head_b, head_b_bytes) = record(vec![root_b.root.hash()], None, 0);
    source.insert(head_a, head_a_bytes);
    source.insert(head_b, head_b_bytes);

    let mut memo = VerificationMemo::new();
    let first = dark_reachability::live_set(
        &mut source,
        &[head_a],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap_err();
    let mut memo = VerificationMemo::new();
    let second = dark_reachability::live_set(
        &mut source,
        &[head_b],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap_err();
    let (root_of_first, error_of_first) = match first {
        ReachError::Composite { root, error } => (root, *error),
        other => panic!("the corrupt shared chunk fails as Composite: {other:?}"),
    };
    let (root_of_second, error_of_second) = match second {
        ReachError::Composite { root, error } => (root, *error),
        other => panic!("the corrupt shared chunk fails as Composite: {other:?}"),
    };
    assert_eq!(root_of_first, root_a.root.hash());
    assert_eq!(root_of_second, root_b.root.hash());
    assert_eq!(
        error_of_first, error_of_second,
        "both roots fail identically on the shared corruption"
    );
    assert!(matches!(
        error_of_first,
        engine_persist::composite_reader::CompositeReadError::HashMismatch {
            identity
        } if identity == shared
    ));
    assert!(
        !memo.contains(&shared),
        "the corrupt shared chunk never earns a memo entry (the pre-corruption prefix may)"
    );
}

#[test]
fn the_walk_respects_the_revision_limit() {
    // A five-record chain under a three-revision limit refuses
    // typed (the bounded traversal).
    let root = synthetic_composite(0, None);
    let mut source = MapSource::new();
    install(&mut source, &root);
    let mut parent = None;
    let mut head = Hash::ZERO;
    for epoch in 0..5 {
        let (identity, bytes) = record(vec![], parent, epoch);
        source.insert(identity, bytes);
        parent = Some(identity);
        head = identity;
    }
    let mut memo = VerificationMemo::new();
    let error = dark_reachability::live_set(
        &mut source,
        &[head],
        &ReachabilityLimits {
            max_revisions: 3,
            max_roots: 16,
        },
        &mut memo,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ReachError::Limit {
            kind: "reachability/revision-count",
            ..
        }
    ));
}

#[test]
fn the_diamond_references_expand_once_and_dedup() {
    // Two revisions on one chain BOTH reference the same composite
    // root: the root expands ONCE (the per-chunk reference counts
    // stay per-DISTINCT-root), and the shared parent tail walks
    // once.
    let root = synthetic_composite(0, None);
    let mut source = MapSource::new();
    install(&mut source, &root);
    let (founding, founding_bytes) = record(vec![root.root.hash()], None, 0);
    let (middle, middle_bytes) = record(vec![root.root.hash()], Some(founding), 1);
    let (head, head_bytes) = record(vec![root.root.hash()], Some(middle), 2);
    source.insert(founding, founding_bytes);
    source.insert(middle, middle_bytes);
    source.insert(head, head_bytes);

    let mut memo = VerificationMemo::new();
    let live: LiveSet = dark_reachability::live_set(
        &mut source,
        &[head],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap();
    assert_eq!(live.composite_roots.len(), 1);
    assert_eq!(live.visited_revisions.len(), 3);
    for count in live.chunk_reference_counts.values() {
        assert_eq!(*count, 1, "the diamond expands the root once");
    }
    assert_eq!(memo.len(), root.manifest.entries.len());
}

// ---------------------------------- the K2-B review correctives A and B

/// One domain-framed opaque object under an arbitrary domain — the
/// fixture for the family-classification matrix.
fn domain_object(domain: &str, seed: u8) -> (Hash, Vec<u8>) {
    let mut enc = engine_core::Encoder::new();
    enc.write_str(domain);
    enc.write_bytes(&[seed; 12]);
    let object = StoredObject::new(enc.into_bytes()).unwrap();
    (object.hash(), object.into_bytes())
}

#[test]
fn the_snapshot_family_classification_fails_closed_on_unsupported_versions() {
    // Corrective A: the walk recognizes the session-snapshot FAMILY
    // — the supported /2 expands, the supported legacy /1 snapshot
    // is a direct object, every OTHER family member (a future /3,
    // /9, or unversioned member) refuses the walk TYPED with the
    // offending root's identity, and unrelated domains stay direct
    // objects. The failed walk produces NO live set — never a
    // partial result that silently omits the unknown format's
    // dependencies.
    let root = synthetic_composite(0, None);
    for (domain, label) in [
        ("engine-slice/session-snapshot/3", "the future /3"),
        ("engine-slice/session-snapshot/9", "another family member"),
        ("engine-slice/session-snapshot/x", "an unversioned member"),
    ] {
        let (future, future_bytes) = domain_object(domain, 5);
        let mut source = MapSource::new();
        install(&mut source, &root);
        source.insert(future, future_bytes);
        let (head, head_bytes) = record(vec![root.root.hash(), future], None, 0);
        source.insert(head, head_bytes);
        let mut memo = VerificationMemo::new();
        let error = dark_reachability::live_set(
            &mut source,
            &[head],
            &ReachabilityLimits::default(),
            &mut memo,
        )
        .unwrap_err();
        assert_eq!(
            error,
            ReachError::UnsupportedSnapshot {
                identity: future,
                domain: domain.to_owned(),
            },
            "{label}: the recognized-but-unsupported family member refuses with its identity"
        );
    }
}

#[test]
fn the_unsupported_family_member_in_a_historical_revision_fails_the_walk() {
    // Corrective A's historical case: the unsupported family member
    // rides an ANCESTOR revision (not the retained head) — the
    // parent walk reaches it and the whole walk refuses; the
    // head-only classification would have missed it entirely.
    let (future, future_bytes) = domain_object("engine-slice/session-snapshot/3", 5);
    let mut source = MapSource::new();
    source.insert(future, future_bytes);
    let (founding, founding_bytes) = record(vec![future], None, 0);
    let (head, head_bytes) = record(vec![], Some(founding), 1);
    source.insert(founding, founding_bytes);
    source.insert(head, head_bytes);
    let mut memo = VerificationMemo::new();
    let error = dark_reachability::live_set(
        &mut source,
        &[head],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ReachError::UnsupportedSnapshot {
            identity: future,
            domain: "engine-slice/session-snapshot/3".to_owned(),
        },
        "the historical family member refuses with its identity"
    );
}

#[test]
fn the_supported_legacy_snapshot_and_unrelated_domains_stay_direct() {
    // The family's SUPPORTED legacy /1 snapshot (the whole-object
    // form) classifies conservatively as a direct object — and is
    // NEVER retirement-eligible as a candidate; unrelated domains
    // keep the direct-object behavior; a malformed (domain-less)
    // object fails closed.
    let (legacy_snapshot, legacy_bytes) = domain_object("engine-slice/session-snapshot/1", 7);
    let (blob, blob_bytes) = domain_object("sektor7/save/1", 9);
    let root = synthetic_composite(0, None);
    let mut source = MapSource::new();
    install(&mut source, &root);
    source.insert(legacy_snapshot, legacy_bytes);
    source.insert(blob, blob_bytes);
    let (head, head_bytes) = record(vec![legacy_snapshot, blob], None, 0);
    source.insert(head, head_bytes);
    let mut memo = VerificationMemo::new();
    let live = dark_reachability::live_set(
        &mut source,
        &[head],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap();
    assert_eq!(
        live.direct_objects,
        [legacy_snapshot, blob].into_iter().collect(),
        "the supported legacy /1 snapshot and the unrelated blob are direct objects"
    );
    // Corrective B: the live legacy snapshot is never eligible.
    assert_eq!(
        live.retirement_eligibility(RetirementCandidate(legacy_snapshot)),
        RetirementEligibility::Retained(RetentionReason::DirectObject),
        "a live legacy /1 snapshot is never retirement-eligible"
    );
    assert_eq!(
        live.retirement_eligibility(RetirementCandidate(blob)),
        RetirementEligibility::Retained(RetentionReason::DirectObject),
        "a live direct object referenced by a retained record stays protected"
    );

    // The malformed object: no decidable domain at all.
    let mut malformed = Vec::new();
    malformed.extend_from_slice(&[0xffu8; 24]);
    let malformed_identity = engine_persist::hash::hash_object(&malformed);
    let mut source = MapSource::new();
    source.insert(malformed_identity, malformed);
    let (head, head_bytes) = record(vec![malformed_identity], None, 0);
    source.insert(head, head_bytes);
    let mut memo = VerificationMemo::new();
    let error = dark_reachability::live_set(
        &mut source,
        &[head],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ReachError::MalformedObject { identity } if identity == malformed_identity
    ));
}

#[test]
fn the_retirement_eligibility_is_invariant_to_ordering_and_duplicates() {
    // Corrective B: the per-candidate verdict is a pure function of
    // the completed live set — the same candidates in any order,
    // duplicated, yield identical verdicts.
    let (shared, shared_bytes) = shared_chunk();
    let root_a = synthetic_composite(0, Some(shared));
    let root_b = synthetic_composite(60, Some(shared));
    let mut source = MapSource::new();
    install(&mut source, &root_a);
    install(&mut source, &root_b);
    source.insert(shared, shared_bytes);
    let (head_a, head_a_bytes) = record(vec![root_a.root.hash()], None, 0);
    let (head_b, head_b_bytes) = record(vec![root_b.root.hash()], None, 0);
    source.insert(head_a, head_a_bytes);
    source.insert(head_b, head_b_bytes);
    let mut memo = VerificationMemo::new();
    let live = dark_reachability::live_set(
        &mut source,
        &[head_a, head_b],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap();
    let candidates = [
        RetirementCandidate(root_a.root.hash()),
        RetirementCandidate(root_b.root.hash()),
        RetirementCandidate(shared),
        RetirementCandidate(head_a),
    ];
    let first: Vec<_> = candidates
        .iter()
        .map(|c| live.retirement_eligibility(*c))
        .collect();
    let mut reordered = candidates.to_vec();
    reordered.reverse();
    let second: Vec<_> = reordered
        .iter()
        .map(|c| live.retirement_eligibility(*c))
        .collect();
    // Pair each verdict with its candidate so the reordering
    // comparison is per-candidate, not positional.
    let by_candidate = |verdicts: &[(RetirementCandidate, RetirementEligibility)]| {
        let mut v = verdicts.to_vec();
        v.sort_by_key(|(candidate, _)| candidate.0);
        v
    };
    let paired_first: Vec<_> = candidates
        .iter()
        .zip(&first)
        .map(|(c, v)| (*c, *v))
        .collect();
    let paired_second: Vec<_> = reordered
        .iter()
        .zip(&second)
        .map(|(c, v)| (*c, *v))
        .collect();
    assert_eq!(by_candidate(&paired_first), by_candidate(&paired_second));
    // Duplicates: the same candidate asked twice answers identically.
    for candidate in &candidates {
        assert_eq!(
            live.retirement_eligibility(*candidate),
            live.retirement_eligibility(*candidate)
        );
    }
}
