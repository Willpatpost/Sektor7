//! The S6-C4 Phase 2 K2-B review's ADDITIONAL COVERAGE — the
//! direct-versus-indirect publication-reference distinction, as a
//! PURE protocol-level fixture set. **No production append path is
//! modified**: these fixtures demonstrate WHY a direct
//! `record.content_refs ∩ reservation` check is INSUFFICIENT for
//! the future composite admission, and WHAT the admission policy
//! must instead check — the AUTHENTICATED dependency closure (the
//! manifest's declared set, verified where the bytes are available)
//! — so that an admission whose closure intersects a hypothetical
//! non-terminal reclamation audit's reservation is rejected or
//! conservatively blocked.
//!
//! **The modeled policy** (a design fixture, NOT an
//! implementation): an admission of a composite root is REJECTED
//! iff its authenticated dependency closure intersects the active
//! reservation, and CONSERVATIVELY BLOCKED iff the closure cannot
//! be authenticated (missing, corrupt, or unverifiable
//! dependencies). Two authentication paths are modeled, mirroring
//! the future `append_composite()`'s two supply forms:
//!
//! - **NEW SUPPLY** — the caller supplies the root and the chunk
//!   objects: the K2-A `validate_composite_closure` authenticates
//!   the supplied bytes against the manifest, and the DECLARED set
//!   (new ∪ reused — content addressing makes a reused identity
//!   and a same-hash new supply the same identity) is what fences;
//! - **ALREADY-STORED reintroduction** — the root is read back from
//!   the store by identity: the K2-B dark reader authenticates the
//!   full closure from the stored bytes, and the VERIFIED
//!   dependency set is what fences.
//!
//! **The open production decision (explicitly NOT chosen here):**
//! whether the future composite admission is a DEDICATED
//! `append_composite()` API (with the generic `append()` refusing
//! `/2`-domain objects — the domain gate) or an
//! authenticated-closure check inside the generic `append()`. The
//! K2-B delivery recommended the domain gate; the owner decides.
//! These fixtures establish only the CHECK SEMANTICS both designs
//! must implement.

use std::collections::{BTreeSet, HashMap};

use engine_persist::StoredObject;
use engine_persist::composite_manifest::{self, CompositeClosure};
use engine_persist::composite_reader::{self, ObjectSource, ObjectSourceError, ReadLimits};
use engine_persist::hash::Hash;
use engine_persist::record::RevisionRecord;

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
/// synthetic chunk envelopes), with one designated chunk other
/// composites may share. The SHARED chunk's object is NOT in
/// `chunk_objects` — the caller supplies it (the store or the
/// admission's new-chunk set).
struct SyntheticComposite {
    root: StoredObject,
    chunk_objects: Vec<(Hash, Vec<u8>)>,
}

fn synthetic_composite(variant: u8, shared: Option<(Hash, Vec<u8>)>) -> SyntheticComposite {
    let mut seed = variant;
    let mut entries = Vec::new();
    let mut chunk_objects = Vec::new();
    let push_singleton = |entries: &mut Vec<composite_manifest::ManifestEntry>,
                          chunk_objects: &mut Vec<(Hash, Vec<u8>)>,
                          seed: &mut u8,
                          tag: &'static str| {
        let payload = vec![*seed; 10 + *seed as usize];
        *seed = seed.wrapping_add(1);
        let envelope = composite_manifest::encode_chunk_envelope(tag, &payload);
        entries.push(composite_manifest::ManifestEntry {
            tag,
            identity: envelope.hash(),
            payload_length: payload.len() as u64,
            kind: composite_manifest::SectionKind::Singleton,
        });
        chunk_objects.push((envelope.hash(), envelope.into_bytes()));
    };
    for tag in [
        composite_manifest::TAG_HEADER,
        composite_manifest::TAG_NEAR,
        composite_manifest::TAG_FAR,
        composite_manifest::TAG_BLOCKED,
        composite_manifest::TAG_HELD,
        composite_manifest::TAG_LEDGER_META,
    ] {
        push_singleton(&mut entries, &mut chunk_objects, &mut seed, tag);
    }
    let sealed_width = 4u64 * 31;
    for sealed in 0..2usize {
        let identity = match shared.as_ref().filter(|_| sealed == 0) {
            Some((identity, _)) => *identity,
            None => {
                let payload = vec![seed; sealed_width as usize];
                seed = seed.wrapping_add(1);
                let envelope = composite_manifest::encode_chunk_envelope(
                    composite_manifest::TAG_LEDGER_RECORDS,
                    &payload,
                );
                let identity = envelope.hash();
                chunk_objects.push((identity, envelope.into_bytes()));
                identity
            }
        };
        entries.push(composite_manifest::ManifestEntry {
            tag: composite_manifest::TAG_LEDGER_RECORDS,
            identity,
            payload_length: sealed_width,
            kind: composite_manifest::SectionKind::LedgerSealed,
        });
    }
    let tail_bytes = 2u64 * 31;
    let payload = vec![seed; tail_bytes as usize];
    let envelope =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_LEDGER_RECORDS, &payload);
    entries.push(composite_manifest::ManifestEntry {
        tag: composite_manifest::TAG_LEDGER_RECORDS,
        identity: envelope.hash(),
        payload_length: tail_bytes,
        kind: composite_manifest::SectionKind::LedgerTail,
    });
    chunk_objects.push((envelope.hash(), envelope.into_bytes()));
    for tag in [
        composite_manifest::TAG_WORLD,
        composite_manifest::TAG_DIRECTOR,
    ] {
        push_singleton(&mut entries, &mut chunk_objects, &mut seed, tag);
    }
    let composed_length = entries.iter().map(|entry| entry.payload_length).sum();
    let manifest = composite_manifest::CompositeManifest {
        history_kind: composite_manifest::HistoryKind::Full,
        ledger_boundary: 4,
        composed_length,
        entries,
    };
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    SyntheticComposite {
        root,
        chunk_objects,
    }
}

fn shared_chunk() -> (Hash, Vec<u8>) {
    let payload = vec![77u8; 4 * 31];
    let envelope =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_LEDGER_RECORDS, &payload);
    (envelope.hash(), envelope.into_bytes())
}

// ------------------------------------------------------- the policy model

/// The admission verdict under the modeled policy.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AdmissionVerdict {
    /// The authenticated closure does not intersect the
    /// reservation — the admission may proceed (the model grants
    /// nothing; the future production path decides).
    Admitted { closure: Vec<Hash> },
    /// The AUTHENTICATED closure intersects the reservation: the
    /// admission is rejected, naming the reserved identities.
    Rejected { reserved: Vec<Hash> },
    /// The closure could not be authenticated (missing, corrupt, or
    /// unverifiable dependencies): the admission is conservatively
    /// blocked — never granted on an unverified closure.
    BlockedUnverified { detail: String },
}

/// The ALREADY-STORED reintroduction path: the root is read back
/// from the store by identity and the K2-B dark reader
/// authenticates the FULL closure from the stored bytes — the
/// verified dependency set is what fences.
fn admit_stored(
    source: &mut MapSource,
    root_identity: &Hash,
    reservation: &BTreeSet<Hash>,
) -> AdmissionVerdict {
    let root_bytes = match source.read_object(root_identity) {
        Ok(bytes) => bytes,
        Err(error) => {
            return AdmissionVerdict::BlockedUnverified {
                detail: format!("the root could not be read: {error:?}"),
            };
        }
    };
    let root = match StoredObject::new(root_bytes) {
        Ok(root) => root,
        Err(error) => {
            return AdmissionVerdict::BlockedUnverified {
                detail: format!("the root could not be framed: {error:?}"),
            };
        }
    };
    let verified =
        match composite_reader::read_composite_closure(source, &root, &ReadLimits::default()) {
            Ok(verified) => verified,
            Err(error) => {
                return AdmissionVerdict::BlockedUnverified {
                    detail: format!("the closure could not be authenticated: {error}"),
                };
            }
        };
    let closure: Vec<Hash> = verified.dependency_identities();
    let reserved: Vec<Hash> = closure
        .iter()
        .filter(|identity| reservation.contains(*identity))
        .copied()
        .collect();
    if reserved.is_empty() {
        AdmissionVerdict::Admitted { closure }
    } else {
        AdmissionVerdict::Rejected { reserved }
    }
}

/// The NEW-SUPPLY path: the caller supplies the root and the chunk
/// objects (plus the REUSED identities); the K2-A pure closure
/// validator authenticates the supplied bytes against the manifest
/// and the record's root reference — and the DECLARED set (new ∪
/// reused) is what fences (content addressing makes a reused
/// identity and a same-hash new supply the same identity).
fn admit_supplied(
    record: &RevisionRecord,
    closure: &CompositeClosure,
    reservation: &BTreeSet<Hash>,
) -> AdmissionVerdict {
    let validated = match composite_manifest::validate_composite_closure(record, closure) {
        Ok(validated) => validated,
        Err(error) => {
            return AdmissionVerdict::BlockedUnverified {
                detail: format!("the supplied closure could not be authenticated: {error:?}"),
            };
        }
    };
    let mut declared = validated.new_identities.clone();
    declared.extend(validated.unverified_dependencies.iter().copied());
    let reserved: Vec<Hash> = declared
        .iter()
        .filter(|identity| reservation.contains(*identity))
        .copied()
        .collect();
    if reserved.is_empty() {
        AdmissionVerdict::Admitted { closure: declared }
    } else {
        AdmissionVerdict::Rejected { reserved }
    }
}

// ----------------------------------------------------------------- tests

#[test]
fn the_direct_reference_check_is_insufficient_the_indirect_closure_fences() {
    // THE core fixture: a revision directly naming a composite root
    // whose authenticated manifest names the reserved chunk X. The
    // DIRECT check (record.content_refs ∩ reservation) sees only
    // the root — it PASSES; the authenticated closure contains X —
    // the modeled admission REJECTS. This is why the future
    // composite admission must check the closure, never the
    // content_refs alone.
    let (shared, shared_bytes) = shared_chunk();
    let composite = synthetic_composite(0, Some((shared, shared_bytes.clone())));
    let mut source = MapSource::new();
    source.insert(composite.root.hash(), composite.root.bytes().to_vec());
    for (identity, bytes) in &composite.chunk_objects {
        source.insert(*identity, bytes.clone());
    }
    source.insert(shared, shared_bytes.clone());

    // The hypothetical non-terminal reclamation audit reserves X.
    let reservation: BTreeSet<Hash> = [shared].into_iter().collect();

    // The revision names ONLY the root.
    let record = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "k2b-admission",
        0,
    )
    .with_content_refs(vec![composite.root.hash()]);

    // The DIRECT check's blind spot, demonstrated: the record's
    // content_refs do NOT intersect the reservation.
    assert!(
        record
            .content_refs
            .iter()
            .all(|identity| !reservation.contains(identity)),
        "the direct content_refs check would PASS — the root itself is not reserved"
    );

    // The authenticated closure check rejects with X.
    assert_eq!(
        admit_stored(&mut source, &composite.root.hash(), &reservation),
        AdmissionVerdict::Rejected {
            reserved: vec![shared]
        },
        "the authenticated indirect closure intersects the reservation"
    );
    // And so does the supplied form (the same root, newly supplied —
    // the shared chunk's object joins the new-chunk set: the K2-A
    // exact-set rule requires the supplied set to match the
    // declared closure).
    let mut new_chunks: Vec<StoredObject> = composite
        .chunk_objects
        .iter()
        .map(|(_, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    new_chunks.push(StoredObject::new(shared_bytes.clone()).unwrap());
    let closure = CompositeClosure {
        root: composite.root.clone(),
        new_chunks,
        reused_chunks: Vec::new(),
    };
    assert_eq!(
        admit_supplied(&record, &closure, &reservation),
        AdmissionVerdict::Rejected {
            reserved: vec![shared]
        },
        "the supplied closure's declared set intersects the reservation"
    );
}

#[test]
fn the_admission_passes_when_nothing_intersects() {
    // The negative control: an unreserved closure admits on both
    // paths — the policy blocks only the intersecting admissions.
    let composite = synthetic_composite(10, None);
    let mut source = MapSource::new();
    source.insert(composite.root.hash(), composite.root.bytes().to_vec());
    for (identity, bytes) in &composite.chunk_objects {
        source.insert(*identity, bytes.clone());
    }
    let reservation: BTreeSet<Hash> = [Hash::from_bytes(&[99u8; 32]).unwrap()]
        .into_iter()
        .collect();
    let record = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "k2b-admission",
        0,
    )
    .with_content_refs(vec![composite.root.hash()]);
    let verdict = admit_stored(&mut source, &composite.root.hash(), &reservation);
    assert!(matches!(verdict, AdmissionVerdict::Admitted { .. }));

    let new_chunks: Vec<StoredObject> = composite
        .chunk_objects
        .iter()
        .map(|(_, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    let closure = CompositeClosure {
        root: composite.root.clone(),
        new_chunks,
        reused_chunks: Vec::new(),
    };
    let verdict = admit_supplied(&record, &closure, &reservation);
    assert!(matches!(verdict, AdmissionVerdict::Admitted { .. }));
}

#[test]
fn the_shared_chunk_and_both_retained_roots_are_fenced() {
    // Two roots share X; X becomes reserved by the hypothetical
    // audit. BOTH roots' admissions reject on X — the shared
    // chunk's liveness through the OTHER retained root is evidence
    // for the audit's future reconciliation (the retirement
    // model's input), while the fence holds for new admissions.
    let (shared, shared_bytes) = shared_chunk();
    let root_a = synthetic_composite(0, Some((shared, shared_bytes.clone())));
    let root_b = synthetic_composite(60, Some((shared, shared_bytes.clone())));
    let mut source = MapSource::new();
    for composite in [&root_a, &root_b] {
        source.insert(composite.root.hash(), composite.root.bytes().to_vec());
        for (identity, bytes) in &composite.chunk_objects {
            source.insert(*identity, bytes.clone());
        }
    }
    source.insert(shared, shared_bytes.clone());
    let reservation: BTreeSet<Hash> = [shared].into_iter().collect();
    for root in [&root_a, &root_b] {
        assert_eq!(
            admit_stored(&mut source, &root.root.hash(), &reservation),
            AdmissionVerdict::Rejected {
                reserved: vec![shared]
            },
            "both roots sharing the reserved chunk are fenced"
        );
    }
}

#[test]
fn the_mixed_new_and_reused_supply_fences_on_the_declared_set() {
    // BOTH supply forms in one closure: some chunks newly supplied,
    // X carried as a REUSED identity (already stored — an
    // unverified dependency per the K2-A contract). The DECLARED
    // set (new ∪ reused) is what fences: the admission rejects on
    // X even though its bytes were never supplied in this
    // admission.
    let (shared, shared_bytes) = shared_chunk();
    let composite = synthetic_composite(0, Some((shared, shared_bytes.clone())));
    let mut source = MapSource::new();
    source.insert(shared, shared_bytes.clone());
    let record = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "k2b-admission",
        0,
    )
    .with_content_refs(vec![composite.root.hash()]);
    // Every chunk EXCEPT X is newly supplied; X rides as reused.
    let new_chunks: Vec<StoredObject> = composite
        .chunk_objects
        .iter()
        .filter(|(identity, _)| *identity != shared)
        .map(|(_, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    let closure = CompositeClosure {
        root: composite.root.clone(),
        new_chunks,
        reused_chunks: vec![shared],
    };
    let reservation: BTreeSet<Hash> = [shared].into_iter().collect();
    assert_eq!(
        admit_supplied(&record, &closure, &reservation),
        AdmissionVerdict::Rejected {
            reserved: vec![shared]
        },
        "the declared set fences the reused identity exactly like a new supply"
    );
    // With NOTHING reserved, the same mixed closure admits — and
    // the validator carries X as an unverified dependency (never
    // claimed durable).
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_supplied(&record, &closure, &empty);
    match verdict {
        AdmissionVerdict::Admitted { closure } => {
            assert!(closure.contains(&shared));
        }
        other => panic!("the unreserved mixed closure admits: {other:?}"),
    }
}

#[test]
fn the_unverifiable_closure_blocks_conservatively() {
    // An admission whose closure CANNOT be authenticated (a missing
    // or corrupt dependency in the already-stored path; a supplied
    // closure that fails the K2-A validation) is CONSERVATIVELY
    // BLOCKED — never granted on an unverified closure, and never
    // mistaken for a rejection-with-proof either.
    let (shared, shared_bytes) = shared_chunk();
    let composite = synthetic_composite(0, Some((shared, shared_bytes.clone())));
    let mut source = MapSource::new();
    source.insert(composite.root.hash(), composite.root.bytes().to_vec());
    for (identity, bytes) in &composite.chunk_objects {
        source.insert(*identity, bytes.clone());
    }
    source.insert(shared, shared_bytes.clone());
    let reservation: BTreeSet<Hash> = BTreeSet::new();

    // (a) A MISSING dependency: the stored-root read fails closed —
    // the admission blocks (the closure is unauthenticated, so no
    // intersection can be proven either way).
    source.remove(&shared);
    let verdict = admit_stored(&mut source, &composite.root.hash(), &reservation);
    assert!(
        matches!(verdict, AdmissionVerdict::BlockedUnverified { .. }),
        "the missing dependency blocks the admission conservatively: {verdict:?}"
    );

    // (b) A CORRUPT dependency: the same conservative block.
    source.insert(shared, {
        let mut corrupted = vec![77u8; 4 * 31];
        corrupted[0] ^= 0xff;
        let envelope = composite_manifest::encode_chunk_envelope(
            composite_manifest::TAG_LEDGER_RECORDS,
            &corrupted,
        );
        envelope.into_bytes()
    });
    let verdict = admit_stored(&mut source, &composite.root.hash(), &reservation);
    assert!(
        matches!(verdict, AdmissionVerdict::BlockedUnverified { .. }),
        "the corrupt dependency blocks the admission conservatively: {verdict:?}"
    );

    // (c) The SUPPLIED path's unauthenticatable closure: a chunk
    // object whose bytes do not match its manifest entry fails the
    // K2-A validation — the admission blocks.
    let record = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "k2b-admission",
        0,
    )
    .with_content_refs(vec![composite.root.hash()]);
    let mut new_chunks: Vec<StoredObject> = composite
        .chunk_objects
        .iter()
        .map(|(_, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    // Corrupt one supplied chunk's payload (the framing intact, the
    // hash now foreign to the manifest's declared identity).
    let last = new_chunks.last_mut().unwrap();
    let mut bytes = last.bytes().to_vec();
    let end = bytes.len() - 1;
    bytes[end] ^= 0xff;
    *last = StoredObject::new(bytes).unwrap();
    let closure = CompositeClosure {
        root: composite.root.clone(),
        new_chunks,
        reused_chunks: Vec::new(),
    };
    let verdict = admit_supplied(&record, &closure, &reservation);
    assert!(
        matches!(verdict, AdmissionVerdict::BlockedUnverified { .. }),
        "the incoherent supplied closure blocks the admission: {verdict:?}"
    );
}

#[test]
fn the_already_stored_root_reintroduction_is_fenced_like_new_supply() {
    // An ALREADY-STORED composite root reintroduced under a new
    // revision while its closure includes the reserved X: the
    // stored path reads the root back by identity, authenticates
    // the closure from the STORED bytes, and rejects on X — the
    // previously-stored form cannot smuggle a reserved dependency
    // past the admission any more than a new supply can.
    let (shared, shared_bytes) = shared_chunk();
    let composite = synthetic_composite(0, Some((shared, shared_bytes.clone())));
    let mut source = MapSource::new();
    source.insert(composite.root.hash(), composite.root.bytes().to_vec());
    for (identity, bytes) in &composite.chunk_objects {
        source.insert(*identity, bytes.clone());
    }
    source.insert(shared, shared_bytes.clone());
    let reservation: BTreeSet<Hash> = [shared].into_iter().collect();

    // The root was stored BEFORE the reservation existed; the
    // re-admission attempt happens while X is reserved.
    assert_eq!(
        admit_stored(&mut source, &composite.root.hash(), &reservation),
        AdmissionVerdict::Rejected {
            reserved: vec![shared]
        },
        "the previously-stored root's closure fences on the reserved shared chunk"
    );

    // The fence lifts only when the reservation does: with nothing
    // reserved, the same stored root admits.
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_stored(&mut source, &composite.root.hash(), &empty);
    assert!(matches!(verdict, AdmissionVerdict::Admitted { .. }));
}
