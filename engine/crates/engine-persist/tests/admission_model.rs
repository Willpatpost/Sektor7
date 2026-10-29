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
    /// Identities the source FAILS on without proving absence (the
    /// R2 case-5 injection: a transport/provider error).
    unavailable: std::collections::HashSet<Hash>,
}

impl MapSource {
    fn new() -> Self {
        MapSource {
            objects: HashMap::new(),
            unavailable: std::collections::HashSet::new(),
        }
    }

    fn insert(&mut self, identity: Hash, bytes: Vec<u8>) {
        self.objects.insert(identity, bytes);
    }

    fn remove(&mut self, identity: &Hash) {
        self.objects.remove(identity);
    }

    fn mark_unavailable(&mut self, identity: &Hash) {
        self.unavailable.insert(*identity);
    }
}

impl ObjectSource for MapSource {
    fn read_object(&mut self, identity: &Hash) -> Result<Vec<u8>, ObjectSourceError> {
        if self.unavailable.contains(identity) {
            return Err(ObjectSourceError::Unavailable(
                "the model marked this identity unavailable".to_owned(),
            ));
        }
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

/// The admission verdict under the modeled policy. The stages are
/// EXPLICIT about what is and is not proven (the R2 correction):
/// reservation clearance and dependency verification are DISTINCT
/// claims — a fence-clear result never implies that every
/// dependency has been verified, and a hash appearing in a
/// manifest is never evidence that its bytes exist.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AdmissionVerdict {
    /// The COMPLETE dependency closure is authenticated (every
    /// dependency — newly supplied AND reused — verified against
    /// its bytes) and does not intersect the reservation. This
    /// proves exactly: the closure's identities are authentic and
    /// fence-clear AGAINST THIS MODEL'S reservation input. It does
    /// NOT prove canonical durability, publication readiness
    /// beyond this model, or absence of reservations the caller
    /// did not supply.
    VerifiedUnreserved { closure: Vec<Hash> },
    /// The DECLARED closure (new ∪ reused) does not intersect the
    /// reservation, but one or more REUSED dependencies remain
    /// UNVERIFIED — their bytes have not been read or
    /// hash-checked. Reservation clearance is NOT publication
    /// readiness; the unverified identities are named so no caller
    /// can silently promote them to verified.
    ClearedWithUnverifiedDependencies {
        declared: Vec<Hash>,
        unverified: Vec<Hash>,
    },
    /// The declared or authenticated closure intersects the
    /// reservation: the admission is rejected, naming the reserved
    /// identities. A known intersection is rejectable on the
    /// DECLARED identities alone (verified or not).
    Rejected { reserved: Vec<Hash> },
    /// The stored root's returned bytes hash to a DIFFERENT
    /// identity than the one requested (the R1 correction): a
    /// dishonest or defective source cannot substitute a
    /// completely valid wrong root. Both identities are preserved
    /// in the evidence.
    RootIdentityMismatch { requested: Hash, observed: Hash },
    /// The closure (or the root) could not be authenticated —
    /// missing, corrupt, malformed, or unavailable: the admission
    /// is conservatively blocked, never granted on an unverified
    /// closure. A source failure is never proof of reservation
    /// absence.
    BlockedUnverified { detail: String },
}

/// The ALREADY-STORED reintroduction path: the root is read back
/// from the store by identity — its returned bytes are FRAMED and
/// their COMPLETE SHA-256 identity RECALCULATED and compared with
/// the REQUESTED identity (the R1 correction: a substituted valid
/// root refuses, never admits) — and the K2-B dark reader then
/// authenticates the FULL closure from the stored bytes, every
/// chunk hash-verified independently. The verified dependency set
/// is what fences.
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
    // THE R1 identity authentication: the returned bytes' complete
    // SHA-256 (the existing content-addressing primitive — the
    // StoredObject's own hash over its whole canonical bytes) must
    // equal the REQUESTED identity. A mismatch refuses with both
    // identities preserved — never an admission on a wrong root,
    // however valid its closure.
    let observed_identity = root.hash();
    if observed_identity != *root_identity {
        return AdmissionVerdict::RootIdentityMismatch {
            requested: *root_identity,
            observed: observed_identity,
        };
    }
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
        AdmissionVerdict::VerifiedUnreserved { closure }
    } else {
        AdmissionVerdict::Rejected { reserved }
    }
}

/// Verifies ONE reused dependency through the source: the COMPLETE
/// chunk-envelope verification, matching the K2-B reader's fresh
/// path semantics (the R4 correction — hash equality proves the
/// bytes match the declared identity, NEVER that the bytes satisfy
/// the envelope grammar). The bytes are read, framed, hash-verified
/// against the declared identity, and then FULLY decoded: the
/// chunk domain, the section tag vs the manifest entry, the
/// length-prefixed payload under `MAX_CHUNK_BYTES`, the decoded
/// payload length vs the manifest entry's declared
/// `payload_length`, and the complete consumption with no trailing
/// data. A missing, corrupt, malformed, truncated, oversized, or
/// unavailable dependency refuses — an unverified reused identity
/// is never silently promoted to a verified one, and a
/// hash-authentic but structurally invalid chunk never reaches the
/// full-verification verdict.
fn verify_reused_dependency(
    source: &mut MapSource,
    identity: &Hash,
    manifest: &composite_manifest::CompositeManifest,
) -> Result<(), String> {
    let bytes = match source.read_object(identity) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(format!(
                "the reused dependency {identity} could not be read: {error:?}"
            ));
        }
    };
    let object = match StoredObject::new(bytes) {
        Ok(object) => object,
        Err(error) => {
            return Err(format!(
                "the reused dependency {identity} could not be framed: {error:?}"
            ));
        }
    };
    if object.hash() != *identity {
        return Err(format!(
            "the reused dependency {identity} bytes do not hash to its declared identity"
        ));
    }
    let Some(entry) = manifest
        .entries
        .iter()
        .find(|entry| &entry.identity == identity)
    else {
        return Err(format!(
            "the reused dependency {identity} is not declared by the manifest"
        ));
    };
    let mut decoder = engine_core::Decoder::new(object.bytes());
    let observed_domain = decoder
        .read_str(engine_core::wire::MAX_TAG_LEN, "k2b-admission-reused")
        .map_err(|error| format!("the reused dependency {identity} envelope: {error:?}"))?;
    if observed_domain != composite_manifest::SESSION_CHUNK_DOMAIN {
        return Err(format!(
            "the reused dependency {identity} carries the foreign domain {observed_domain}"
        ));
    }
    let observed_tag = decoder
        .read_str(engine_core::wire::MAX_TAG_LEN, "k2b-admission-reused")
        .map_err(|error| format!("the reused dependency {identity} envelope: {error:?}"))?;
    if observed_tag != entry.tag {
        return Err(format!(
            "the reused dependency {identity} is tagged {observed_tag}, the manifest declares {}",
            entry.tag
        ));
    }
    // THE R4 completion — the rest of the envelope, exactly the
    // reader's fresh-path semantics: the length-prefixed payload
    // decoded under the schema's per-chunk bound (no excessive
    // allocation: the decoder bounds the declared length before
    // trusting it), the decoded length checked against the
    // manifest entry's declaration, and the COMPLETE consumption
    // with no trailing data.
    let payload = decoder
        .read_bytes(composite_manifest::MAX_CHUNK_BYTES, "k2b-admission-reused")
        .map_err(|error| format!("the reused dependency {identity} envelope: {error:?}"))?;
    if payload.len() as u64 != entry.payload_length {
        return Err(format!(
            "the reused dependency {identity} carries a {}-byte payload, the manifest declares {}",
            payload.len(),
            entry.payload_length
        ));
    }
    decoder
        .finish()
        .map_err(|error| format!("the reused dependency {identity} envelope: {error:?}"))?;
    Ok(())
}

/// The NEW-SUPPLY path: the caller supplies the root and the chunk
/// objects (plus the REUSED identities); the K2-A pure closure
/// validator authenticates the SUPPLIED bytes against the manifest
/// and the record's root reference — and the DECLARED set (new ∪
/// reused) is what fences (content addressing makes a reused
/// identity and a same-hash new supply the same identity; both
/// fence equally, verified or not).
///
/// The verification completeness is EXPLICIT (the R2 correction):
/// an all-new closure is fully authenticated by the validator; a
/// closure with REUSED identities is fully verified only when
/// `verify_reused_through` supplies a source AND every reused
/// dependency reads, hash-verifies, and envelope-checks against
/// its manifest entry — otherwise the verdict is the DISTINCT
/// fence-clear-but-unverified stage (reservation clearance is NOT
/// publication readiness). The fence check itself uses the
/// declared identities, so a source failure is never interpreted
/// as proof of reservation absence.
fn admit_supplied(
    record: &RevisionRecord,
    closure: &CompositeClosure,
    reservation: &BTreeSet<Hash>,
    verify_reused_through: Option<&mut MapSource>,
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
    // The fence: the DECLARED set (new ∪ reused), verified or not —
    // a known reservation intersection is rejectable on the
    // declared identities alone.
    let reserved: Vec<Hash> = declared
        .iter()
        .filter(|identity| reservation.contains(*identity))
        .copied()
        .collect();
    if !reserved.is_empty() {
        return AdmissionVerdict::Rejected { reserved };
    }
    let unverified: Vec<Hash> = validated.unverified_dependencies.clone();
    if unverified.is_empty() {
        // An all-new closure: the validator authenticated every
        // supplied chunk against the manifest.
        return AdmissionVerdict::VerifiedUnreserved { closure: declared };
    }
    match verify_reused_through {
        None => AdmissionVerdict::ClearedWithUnverifiedDependencies {
            declared,
            unverified,
        },
        Some(source) => {
            for identity in &unverified {
                if let Err(detail) = verify_reused_dependency(source, identity, &validated.manifest)
                {
                    return AdmissionVerdict::BlockedUnverified { detail };
                }
            }
            AdmissionVerdict::VerifiedUnreserved { closure: declared }
        }
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
        admit_supplied(&record, &closure, &reservation, None),
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
    assert!(matches!(
        verdict,
        AdmissionVerdict::VerifiedUnreserved { .. }
    ));

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
    let verdict = admit_supplied(&record, &closure, &reservation, None);
    assert!(matches!(
        verdict,
        AdmissionVerdict::VerifiedUnreserved { .. }
    ));
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
        admit_supplied(&record, &closure, &reservation, None),
        AdmissionVerdict::Rejected {
            reserved: vec![shared]
        },
        "the declared set fences the reused identity exactly like a new supply"
    );
    // With NOTHING reserved, the same mixed closure is
    // fence-CLEAR but NOT publication-ready (the R2 corrected
    // semantics): the reused X has not been read or verified, and
    // the verdict names it — the old test's bare success claim was
    // unjustified and is revised, not renamed.
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_supplied(&record, &closure, &empty, None);
    match verdict {
        AdmissionVerdict::ClearedWithUnverifiedDependencies {
            declared,
            unverified,
        } => {
            assert!(declared.contains(&shared));
            assert_eq!(unverified, vec![shared]);
        }
        other => panic!("the unreserved mixed closure is cleared-but-unverified: {other:?}"),
    }
    // With the source supplied AND X's bytes stored and intact, the
    // same closure reaches the FULL verified stage.
    let verdict = admit_supplied(&record, &closure, &empty, Some(&mut source));
    assert!(matches!(
        verdict,
        AdmissionVerdict::VerifiedUnreserved { .. }
    ));
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
    let verdict = admit_supplied(&record, &closure, &reservation, None);
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
    // reserved, the same stored root reaches the FULL verified
    // stage (the R1 identity check passes, the reader verifies
    // every chunk).
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_stored(&mut source, &composite.root.hash(), &empty);
    assert!(matches!(
        verdict,
        AdmissionVerdict::VerifiedUnreserved { .. }
    ));
}

// --------------------------- the R1 and R2 final-review regressions

#[test]
fn the_r1_substituted_valid_root_refuses_with_both_identities() {
    // R1 cases 2, 3, and 6: a dishonest or defective source returns
    // a DIFFERENT, completely valid composite root's bytes under
    // the requested identity. The substituted root's own closure
    // would verify — but the recalculated SHA-256 of the returned
    // bytes is the WRONG root's identity: the admission refuses
    // with BOTH identities preserved, under an empty reservation
    // AND under a reservation that would otherwise permit it.
    let root_a = synthetic_composite(0, None);
    let root_b = synthetic_composite(60, None);
    assert_ne!(root_a.root.hash(), root_b.root.hash());
    let mut source = MapSource::new();
    // The store serves root B's ENTIRE valid closure — but under
    // root A's identity at the root slot.
    source.insert(root_a.root.hash(), root_b.root.bytes().to_vec());
    for (identity, bytes) in &root_b.chunk_objects {
        source.insert(*identity, bytes.clone());
    }

    let empty: BTreeSet<Hash> = BTreeSet::new();
    assert_eq!(
        admit_stored(&mut source, &root_a.root.hash(), &empty),
        AdmissionVerdict::RootIdentityMismatch {
            requested: root_a.root.hash(),
            observed: root_b.root.hash(),
        },
        "the wrong-identity root refuses even with valid, unreserved dependencies"
    );

    // A reservation that would otherwise permit root A's true
    // closure (reserving an identity in NEITHER closure): the
    // mismatch still refuses — never an admission on a wrong root.
    let permissive: BTreeSet<Hash> = [Hash::from_bytes(&[99u8; 32]).unwrap()]
        .into_iter()
        .collect();
    assert_eq!(
        admit_stored(&mut source, &root_a.root.hash(), &permissive),
        AdmissionVerdict::RootIdentityMismatch {
            requested: root_a.root.hash(),
            observed: root_b.root.hash(),
        },
        "the mismatch refuses under a permissive reservation too"
    );

    // The control: the HONEST store (root B under root B's
    // identity) reaches the full verified stage.
    source.insert(root_b.root.hash(), root_b.root.bytes().to_vec());
    let verdict = admit_stored(&mut source, &root_b.root.hash(), &empty);
    assert!(matches!(
        verdict,
        AdmissionVerdict::VerifiedUnreserved { .. }
    ));
}

#[test]
fn the_r1_missing_or_malformed_root_blocks_conservatively() {
    // R1 cases 4 and 5: the requested root absent from the store,
    // or garbage bytes under its identity — both fail closed
    // BEFORE any identity or closure claim.
    let composite = synthetic_composite(0, None);
    let empty: BTreeSet<Hash> = BTreeSet::new();

    // Missing: the source proves absence.
    let mut source = MapSource::new();
    let verdict = admit_stored(&mut source, &composite.root.hash(), &empty);
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail } if detail.contains("could not be read")),
        "the missing root blocks: {verdict:?}"
    );

    // Malformed: bytes under the identity that do not frame as a
    // domain-tagged object at all.
    let mut source = MapSource::new();
    let garbage = vec![0xffu8; 24];
    source.insert(composite.root.hash(), garbage);
    let verdict = admit_stored(&mut source, &composite.root.hash(), &empty);
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail } if detail.contains("could not be framed")),
        "the malformed root blocks: {verdict:?}"
    );
}

#[test]
fn the_r2_reused_missing_or_corrupt_blocks_the_verified_stage() {
    // R2 cases 3 and 4: a mixed new/reused closure whose reused
    // dependency is MISSING or CORRUPTED in the store never reaches
    // the full verified stage — the fence-clear declared set is
    // not publication readiness, and the verification failure
    // blocks conservatively.
    let (shared, shared_bytes) = shared_chunk();
    let composite = synthetic_composite(0, Some((shared, shared_bytes.clone())));
    let record = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "k2b-admission",
        0,
    )
    .with_content_refs(vec![composite.root.hash()]);
    let new_chunks: Vec<StoredObject> = composite
        .chunk_objects
        .iter()
        .map(|(_, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    let closure = CompositeClosure {
        root: composite.root.clone(),
        new_chunks,
        reused_chunks: vec![shared],
    };
    let empty: BTreeSet<Hash> = BTreeSet::new();

    // (a) The reused dependency MISSING from the store.
    let mut source = MapSource::new();
    let verdict = admit_supplied(&record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail } if detail.contains("could not be read")),
        "the missing reused dependency blocks: {verdict:?}"
    );

    // (b) The reused dependency CORRUPTED: a well-formed envelope
    // under the identity whose bytes hash elsewhere.
    let mut source = MapSource::new();
    let mut corrupted_payload = vec![77u8; 4 * 31];
    corrupted_payload[0] ^= 0xff;
    source.insert(
        shared,
        composite_manifest::encode_chunk_envelope(
            composite_manifest::TAG_LEDGER_RECORDS,
            &corrupted_payload,
        )
        .into_bytes(),
    );
    let verdict = admit_supplied(&record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail } if detail.contains("do not hash")),
        "the corrupt reused dependency blocks: {verdict:?}"
    );

    // The control: the intact reused dependency reaches the full
    // verified stage (R2 case 2).
    let mut source = MapSource::new();
    source.insert(shared, shared_bytes);
    let verdict = admit_supplied(&record, &closure, &empty, Some(&mut source));
    assert!(matches!(
        verdict,
        AdmissionVerdict::VerifiedUnreserved { .. }
    ));
}

#[test]
fn the_r2_source_unavailability_blocks_never_clears() {
    // R2 case 5: the source FAILS for the reused dependency without
    // proving absence — the admission blocks; the source failure is
    // never interpreted as proof of reservation absence (the fence
    // check ran on the declared identities, and the verdict carries
    // the failure).
    let (shared, shared_bytes) = shared_chunk();
    let composite = synthetic_composite(0, Some((shared, shared_bytes.clone())));
    let record = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "k2b-admission",
        0,
    )
    .with_content_refs(vec![composite.root.hash()]);
    let new_chunks: Vec<StoredObject> = composite
        .chunk_objects
        .iter()
        .map(|(_, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    let closure = CompositeClosure {
        root: composite.root.clone(),
        new_chunks,
        reused_chunks: vec![shared],
    };
    let empty: BTreeSet<Hash> = BTreeSet::new();

    let mut source = MapSource::new();
    source.insert(shared, shared_bytes);
    source.mark_unavailable(&shared);
    let verdict = admit_supplied(&record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail } if detail.contains("could not be read")),
        "the unavailable reused dependency blocks conservatively: {verdict:?}"
    );
}

#[test]
fn the_r2_fully_verified_closure_with_a_reserved_chunk_rejects() {
    // R2 case 9: every dependency verifies (the source present, the
    // bytes intact) but ONE chunk is reserved — the fence on the
    // declared set fires regardless of verifiability.
    let (shared, shared_bytes) = shared_chunk();
    let composite = synthetic_composite(0, Some((shared, shared_bytes.clone())));
    let record = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "k2b-admission",
        0,
    )
    .with_content_refs(vec![composite.root.hash()]);
    let new_chunks: Vec<StoredObject> = composite
        .chunk_objects
        .iter()
        .map(|(_, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    let closure = CompositeClosure {
        root: composite.root.clone(),
        new_chunks,
        reused_chunks: vec![shared],
    };
    let reservation: BTreeSet<Hash> = [shared].into_iter().collect();
    let mut source = MapSource::new();
    source.insert(shared, shared_bytes);
    assert_eq!(
        admit_supplied(&record, &closure, &reservation, Some(&mut source)),
        AdmissionVerdict::Rejected {
            reserved: vec![shared]
        },
        "the fully verified closure still fences on the reserved chunk"
    );
    // And without the source (the reused identity unverified): the
    // SAME rejection — a known intersection is rejectable on the
    // declared identities alone (R2 case 7).
    assert_eq!(
        admit_supplied(&record, &closure, &reservation, None),
        AdmissionVerdict::Rejected {
            reserved: vec![shared]
        },
        "the unverified reused identity fences on its declared identity"
    );
}

// ---------------------- the R4 envelope-verification regressions

/// One admission fixture over the synthetic composite with the
/// WORLD singleton's manifest entry REPLACED by an adversarial
/// dependency: the entry carries the adversarial bytes' ACTUAL
/// content identity (the bytes are hash-valid for that identity —
/// the R4 distinction: hash equality, never envelope validity) and
/// the given declared payload length, with the manifest's composed
/// coherence adjusted so the MANIFEST itself stays grammatically
/// valid. The unpatched chunks are the new supply; the adversarial
/// identity rides as reused (or as new, for the equivalence test).
struct AdversarialFixture {
    record: RevisionRecord,
    root: StoredObject,
    /// The unpatched chunk objects (every manifest entry except the
    /// patched world singleton).
    new_chunks: Vec<StoredObject>,
    /// The patched world entry's declared identity — the adversarial
    /// bytes' ACTUAL content identity.
    adversarial_identity: Hash,
}

fn adversarial_world_fixture(
    variant: u8,
    adversarial_bytes: &[u8],
    declared_payload_length: u64,
) -> AdversarialFixture {
    let composite = synthetic_composite(variant, None);
    let mut manifest = composite_manifest::decode(composite.root.bytes()).unwrap();
    let world_index = manifest.entries.len() - 2;
    let original_world_identity = manifest.entries[world_index].identity;
    let original_length = manifest.entries[world_index].payload_length;
    let adversarial_identity = engine_persist::hash::hash_object(adversarial_bytes);
    manifest.entries[world_index].identity = adversarial_identity;
    manifest.entries[world_index].payload_length = declared_payload_length;
    // The composed coherence follows the DECLARED lengths (the
    // subtraction first: the composed total always covers the
    // original entry's length, so no underflow exists).
    manifest.composed_length = manifest.composed_length - original_length + declared_payload_length;
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let record = RevisionRecord::founding(
        engine_persist::record::AuthorityMode::Standalone,
        "k2b-admission-r4",
        0,
    )
    .with_content_refs(vec![root.hash()]);
    let new_chunks: Vec<StoredObject> = composite
        .chunk_objects
        .iter()
        .filter(|(identity, _)| *identity != original_world_identity)
        .map(|(_, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    AdversarialFixture {
        record,
        root,
        new_chunks,
        adversarial_identity,
    }
}

/// Assembles the reused-form closure over an adversarial fixture.
fn reused_closure(fixture: &AdversarialFixture) -> CompositeClosure {
    CompositeClosure {
        root: fixture.root.clone(),
        new_chunks: fixture.new_chunks.clone(),
        reused_chunks: vec![fixture.adversarial_identity],
    }
}

/// The store carrying ONLY the adversarial bytes (the reused
/// dependency's source).
fn adversarial_source(fixture: &AdversarialFixture, bytes: &[u8]) -> MapSource {
    let mut source = MapSource::new();
    source.insert(fixture.adversarial_identity, bytes.to_vec());
    source
}

#[test]
fn the_r4_valid_reused_dependency_reaches_full_verification() {
    // Test A — the positive control: a canonical chunk envelope
    // whose ACTUAL hash and payload length the manifest carries,
    // supplied as a reused dependency with no reservation
    // intersection — the complete envelope verification passes and
    // the admission reaches VerifiedUnreserved.
    let valid_world =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_WORLD, &[5u8; 62]);
    let fixture = adversarial_world_fixture(0, valid_world.bytes(), 62);
    let closure = reused_closure(&fixture);
    let mut source = adversarial_source(&fixture, valid_world.bytes());
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_supplied(&fixture.record, &closure, &empty, Some(&mut source));
    match &verdict {
        AdmissionVerdict::VerifiedUnreserved { closure } => {
            assert!(closure.contains(&fixture.adversarial_identity));
        }
        other => panic!("the valid reused dependency fully verifies: {other:?}"),
    }
}

#[test]
fn the_r4_truncated_payload_framing_blocks_full_verification() {
    // Test B — a reused object whose leading chunk domain and
    // section tag are valid but whose length-prefixed payload is
    // TRUNCATED (the length prefix declares more bytes than the
    // object carries). The object's ACTUAL SHA-256 is in the
    // manifest (hash-valid bytes, structurally invalid envelope) —
    // the payload decode refuses, never VerifiedUnreserved.
    let mut encoder = engine_core::Encoder::new();
    encoder.write_str(composite_manifest::SESSION_CHUNK_DOMAIN);
    encoder.write_str(composite_manifest::TAG_WORLD);
    encoder.write_u64(50);
    let mut bytes = encoder.into_bytes();
    bytes.extend_from_slice(&[0u8; 10]);
    let fixture = adversarial_world_fixture(0, &bytes, 19);
    let closure = reused_closure(&fixture);
    let mut source = adversarial_source(&fixture, &bytes);
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_supplied(&fixture.record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail }
            if detail.contains("envelope")),
        "the truncated payload framing blocks: {verdict:?}"
    );
}

#[test]
fn the_r4_manifest_payload_length_decoherence_blocks_full_verification() {
    // Test C — a structurally VALID chunk envelope (complete
    // framing, payload 93 bytes) whose decoded payload length
    // DISAGREES with the manifest entry's declared payload length
    // (62 — the manifest itself stays valid: the positional
    // grammar holds and the composed coherence is adjusted to the
    // declared lengths). The object's correct SHA-256 is in the
    // manifest. The failure is the payload-length incoherence —
    // not a hash mismatch and not an invalid root manifest.
    let valid_chunk =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_WORLD, &[7u8; 93]);
    let fixture = adversarial_world_fixture(0, valid_chunk.bytes(), 62);
    let closure = reused_closure(&fixture);
    let mut source = adversarial_source(&fixture, valid_chunk.bytes());
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_supplied(&fixture.record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail }
            if detail.contains("93-byte payload") && detail.contains("declares 62")),
        "the payload-length decoherence blocks: {verdict:?}"
    );
}

#[test]
fn the_r4_trailing_bytes_block_full_verification() {
    // Test D — an otherwise valid chunk envelope with EXTRA
    // trailing bytes appended; the manifest carries the EXTENDED
    // bytes' correct SHA-256 and the true payload length, so the
    // domain, tag, and payload checks all pass — the decoder's
    // finish check catches the trailing data.
    let valid_chunk =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_WORLD, &[3u8; 62]);
    let mut bytes = valid_chunk.bytes().to_vec();
    bytes.extend_from_slice(&[0xeeu8; 5]);
    let fixture = adversarial_world_fixture(0, &bytes, 62);
    let closure = reused_closure(&fixture);
    let mut source = adversarial_source(&fixture, &bytes);
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_supplied(&fixture.record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail }
            if detail.contains("envelope")),
        "the trailing bytes block through the finish check: {verdict:?}"
    );
}

#[test]
fn the_r4_oversized_payload_declaration_blocks_without_excess_allocation() {
    // Test E — the maximum-payload bound: a compact adversarial
    // framing whose length prefix declares MORE than
    // MAX_CHUNK_BYTES with only a few actual bytes (no excessive
    // buffer). The bounded decoder refuses the declared length
    // before trusting it — no panic, no oversized allocation.
    let mut encoder = engine_core::Encoder::new();
    encoder.write_str(composite_manifest::SESSION_CHUNK_DOMAIN);
    encoder.write_str(composite_manifest::TAG_WORLD);
    encoder.write_u64(composite_manifest::MAX_CHUNK_BYTES + 1);
    let mut bytes = encoder.into_bytes();
    bytes.extend_from_slice(&[0u8; 4]);
    let fixture = adversarial_world_fixture(0, &bytes, 4);
    let closure = reused_closure(&fixture);
    let mut source = adversarial_source(&fixture, &bytes);
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_supplied(&fixture.record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail }
            if detail.contains("envelope")),
        "the oversized payload declaration blocks: {verdict:?}"
    );
}

#[test]
fn the_r4_wrong_domain_and_wrong_tag_remain_blocked() {
    // Test F — the pre-existing failure classes stay fail-closed
    // alongside the new envelope checks (the missing, corrupt, and
    // unavailable classes are re-proven by the R2 tests): a
    // well-framed object under a FOREIGN domain, and a
    // chunk-domain object carrying the WRONG section tag — both
    // with their ACTUAL hashes in the manifest.
    // (a) The foreign domain.
    let mut encoder = engine_core::Encoder::new();
    encoder.write_str("sektor7/save/1");
    encoder.write_str(composite_manifest::TAG_WORLD);
    encoder.write_bytes(&[1u8; 62]);
    let foreign = encoder.into_bytes();
    let fixture = adversarial_world_fixture(0, &foreign, 62);
    let closure = reused_closure(&fixture);
    let mut source = adversarial_source(&fixture, &foreign);
    let empty: BTreeSet<Hash> = BTreeSet::new();
    let verdict = admit_supplied(&fixture.record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail }
            if detail.contains("foreign domain")),
        "the foreign domain blocks: {verdict:?}"
    );
    // (b) The wrong section tag: the grammar pins the manifest
    // entry's tag to TAG_WORLD; the stored envelope carries
    // TAG_NEAR with a matching payload length.
    let mis_tagged =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_NEAR, &[2u8; 62]);
    let fixture = adversarial_world_fixture(10, mis_tagged.bytes(), 62);
    let closure = reused_closure(&fixture);
    let mut source = adversarial_source(&fixture, mis_tagged.bytes());
    let verdict = admit_supplied(&fixture.record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&verdict, AdmissionVerdict::BlockedUnverified { detail }
            if detail.contains("is tagged")),
        "the wrong section tag blocks: {verdict:?}"
    );
}

#[test]
fn the_r4_reservation_fencing_still_precedes_the_envelope_verification() {
    // Test G — the fence is INDEPENDENT of the closure
    // verification: a declared reused dependency intersecting the
    // active reservation produces the reservation REJECTION even
    // though its bytes are structurally invalid (unverified) — the
    // envelope-verification improvement did not weaken the fence,
    // and the fence did not need the bytes.
    let mut encoder = engine_core::Encoder::new();
    encoder.write_str(composite_manifest::SESSION_CHUNK_DOMAIN);
    encoder.write_str(composite_manifest::TAG_WORLD);
    encoder.write_u64(50);
    let mut bytes = encoder.into_bytes();
    bytes.extend_from_slice(&[0u8; 10]);
    let fixture = adversarial_world_fixture(0, &bytes, 19);
    let closure = reused_closure(&fixture);
    let reservation: BTreeSet<Hash> = [fixture.adversarial_identity].into_iter().collect();
    // Without the source (unverified) and with it (the verification
    // would refuse): the fence rejection is the verdict either way.
    assert_eq!(
        admit_supplied(&fixture.record, &closure, &reservation, None),
        AdmissionVerdict::Rejected {
            reserved: vec![fixture.adversarial_identity]
        }
    );
    let mut source = adversarial_source(&fixture, &bytes);
    assert_eq!(
        admit_supplied(&fixture.record, &closure, &reservation, Some(&mut source)),
        AdmissionVerdict::Rejected {
            reserved: vec![fixture.adversarial_identity]
        },
        "the reservation rejection fires before and independent of the envelope verification"
    );
}

#[test]
fn the_r4_new_and_reused_paths_enforce_equivalent_envelope_correctness() {
    // Test H — the SAME structurally invalid chunk (the truncated
    // payload framing) must not be accepted merely because it is
    // categorized as NEW instead of reused: the new-supply path's
    // K2-A validator and the reused path's completed verifier
    // enforce the equivalent envelope correctness, and both block.
    let mut encoder = engine_core::Encoder::new();
    encoder.write_str(composite_manifest::SESSION_CHUNK_DOMAIN);
    encoder.write_str(composite_manifest::TAG_WORLD);
    encoder.write_u64(50);
    let mut bytes = encoder.into_bytes();
    bytes.extend_from_slice(&[0u8; 10]);
    let fixture = adversarial_world_fixture(0, &bytes, 19);
    let empty: BTreeSet<Hash> = BTreeSet::new();

    // The REUSED form: the completed verifier refuses the framing.
    let closure = reused_closure(&fixture);
    let mut source = adversarial_source(&fixture, &bytes);
    let reused_verdict = admit_supplied(&fixture.record, &closure, &empty, Some(&mut source));
    assert!(
        matches!(&reused_verdict, AdmissionVerdict::BlockedUnverified { .. }),
        "the reused form blocks: {reused_verdict:?}"
    );

    // The NEW form: the K2-A validator's own envelope check refuses
    // the same bytes supplied as a new chunk.
    let mut new_chunks = fixture.new_chunks.clone();
    new_chunks.push(StoredObject::new(bytes.clone()).unwrap());
    let closure = CompositeClosure {
        root: fixture.root.clone(),
        new_chunks,
        reused_chunks: Vec::new(),
    };
    let new_verdict = admit_supplied(&fixture.record, &closure, &empty, None);
    assert!(
        matches!(&new_verdict, AdmissionVerdict::BlockedUnverified { .. }),
        "the new form blocks on the same structurally invalid chunk: {new_verdict:?}"
    );
}
