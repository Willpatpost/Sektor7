//! The S6-C4 K2-C1 COMPOSITE APPEND FOUNDATION — the dedicated,
//! fail-closed production admission for `engine-slice/session-
//! snapshot/2` composite publications. **K2-C1 builds the interface
//! and its verification contract ONLY**: no production caller exists
//! (the covering publisher, the recovery selector, and the
//! reclamation path are untouched — the activation restriction);
//! the generic `append` gains the snapshot-family domain gate so a
//! `/2` root (or any unrecognized future family member) can NEVER
//! enter the chain outside this contract.
//!
//! **The production record shape:** the composite revision's
//! `content_refs` name the root AND every newly supplied chunk (the
//! medium's own invariant — the supplied object set equals the
//! referenced set — so the plain CAS `append` executes the
//! publication with ALL of its existing guarantees); the REUSED
//! chunks are already stored, verified through the medium, and
//! referenced transitively through the manifest. The root identity
//! must be among the references (the snapshot binding).
//!
//! **The admission** (the K2-A/K2-B closed-pass contracts, now the
//! production semantics): the root manifest strictly decodes; the
//! root is referenced by the revision; the declared dependency set
//! matches the supplied new chunks plus the reused identities
//! exactly; EVERY chunk — newly supplied AND reused — is fully
//! verified: its content identity (the SHA-256 over its actual
//! bytes), its envelope structure (the chunk domain, the section
//! tag vs the manifest entry, the length-prefixed payload under the
//! schema's per-chunk bound, the manifest's declared payload
//! length, and the complete consumption with no trailing data —
//! the R4 discipline: hash equality proves the bytes match the
//! identity, NEVER the envelope grammar). Missing, corrupt,
//! malformed, or unavailable dependencies refuse typed BEFORE any
//! mutation; the publication then delegates to the provider's own
//! `append`, preserving the CAS, idempotency, and reconciliation
//! guarantees exactly — an uncertain outcome is never converted
//! into a success.

use engine_core::wire::MAX_TAG_LEN;
use engine_core::{Decoder, SaveError};

use crate::composite_manifest::{self, CompositeManifest, MAX_CHUNK_BYTES, SESSION_CHUNK_DOMAIN};
use crate::hash::Hash;
use crate::medium::{MediumError, StoredObject};
use crate::record::RevisionRecord;

/// One composite publication request: the revision record (whose
/// content references must name the root and every newly supplied
/// chunk), the root manifest object, the newly supplied chunk
/// objects, and the REUSED chunk identities (already stored —
/// verified through the medium before any mutation, never trusted
/// on their hash alone).
#[derive(Debug, Clone)]
pub struct CompositeAppend<'a> {
    pub record: &'a RevisionRecord,
    pub root: &'a StoredObject,
    pub new_chunks: &'a [StoredObject],
    pub reused_chunks: &'a [Hash],
}

/// The admission's typed refusal (mapped onto the medium's error
/// surface; every failure names what was proven).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositeAdmissionError {
    /// The strict root-manifest decode failed (the grammar, the
    /// version, the domain family, the bounds, the coherence).
    Manifest(SaveError),
    /// The revision does not reference the root identity (the
    /// snapshot binding).
    RootNotReferenced { root: Hash },
    /// The declared dependency set does not match the supplied new
    /// chunks plus the reused identities exactly (a duplicate,
    /// omitted, or extraneous dependency).
    DependencySet { detail: String },
    /// A chunk dependency could not be established: missing,
    /// corrupt, malformed, unavailable, or incoherent with its
    /// manifest entry — nothing was mutated.
    Dependency { identity: Hash, detail: String },
}

impl CompositeAdmissionError {
    /// The medium-error form (the typed production surface).
    pub fn into_medium_error(self) -> MediumError {
        let detail = match self {
            CompositeAdmissionError::Manifest(error) => {
                format!("the root manifest failed the strict decode: {error:?}")
            }
            CompositeAdmissionError::RootNotReferenced { root } => {
                format!("the revision does not reference the root {}", root.hex())
            }
            CompositeAdmissionError::DependencySet { detail } => detail,
            CompositeAdmissionError::Dependency { identity, detail } => format!(
                "the chunk dependency {} could not be established: {detail}",
                identity.hex()
            ),
        };
        MediumError::CompositeAdmission { detail }
    }
}

impl std::fmt::Display for CompositeAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompositeAdmissionError::Manifest(error) => {
                write!(f, "the root manifest failed the strict decode: {error}")
            }
            CompositeAdmissionError::RootNotReferenced { root } => {
                write!(f, "the revision does not reference the root {}", root.hex())
            }
            CompositeAdmissionError::DependencySet { detail } => write!(f, "{detail}"),
            CompositeAdmissionError::Dependency { identity, detail } => write!(
                f,
                "the chunk dependency {} could not be established: {detail}",
                identity.hex()
            ),
        }
    }
}

/// Fully verifies ONE chunk dependency's bytes against its manifest
/// entry — the SAME discipline for newly supplied and reused chunks
/// (the R4 equivalence): the content identity (the SHA-256 over the
/// actual bytes equals the declared identity), the chunk domain,
/// the section tag, the length-prefixed payload under the schema's
/// per-chunk bound, the manifest's declared payload length, and the
/// complete consumption with no trailing data.
pub fn verify_chunk_dependency(
    bytes: &[u8],
    identity: &Hash,
    manifest: &CompositeManifest,
) -> Result<(), CompositeAdmissionError> {
    let refuse = |detail: String| CompositeAdmissionError::Dependency {
        identity: *identity,
        detail,
    };
    let object = StoredObject::new(bytes.to_vec())
        .map_err(|error| refuse(format!("the bytes do not frame as an object: {error:?}")))?;
    if object.hash() != *identity {
        return Err(refuse(
            "the bytes do not hash to the declared identity".to_owned(),
        ));
    }
    let Some(entry) = manifest
        .entries
        .iter()
        .find(|entry| &entry.identity == identity)
    else {
        return Err(refuse(
            "the identity is not declared by the manifest".to_owned(),
        ));
    };
    let mut decoder = Decoder::new(object.bytes());
    let observed_domain = decoder
        .read_str(MAX_TAG_LEN, "k2c1-chunk")
        .map_err(|error| refuse(format!("the envelope does not decode: {error:?}")))?;
    if observed_domain != SESSION_CHUNK_DOMAIN {
        return Err(refuse(format!(
            "the envelope carries the foreign domain {observed_domain}"
        )));
    }
    let observed_tag = decoder
        .read_str(MAX_TAG_LEN, "k2c1-chunk")
        .map_err(|error| refuse(format!("the envelope does not decode: {error:?}")))?;
    if observed_tag != entry.tag {
        return Err(refuse(format!(
            "the envelope is tagged {observed_tag}, the manifest declares {}",
            entry.tag
        )));
    }
    let payload = decoder
        .read_bytes(MAX_CHUNK_BYTES, "k2c1-chunk")
        .map_err(|error| refuse(format!("the payload framing is invalid: {error:?}")))?;
    if payload.len() as u64 != entry.payload_length {
        return Err(refuse(format!(
            "the envelope carries a {}-byte payload, the manifest declares {}",
            payload.len(),
            entry.payload_length
        )));
    }
    decoder
        .finish()
        .map_err(|error| refuse(format!("the object carries trailing data: {error:?}")))?;
    Ok(())
}

/// The full admission (pure over the request; the reused
/// dependencies are established through the caller-supplied byte
/// reader — the medium's hash-verified `read_object`). Returns the
/// validated manifest on success; every failure is typed and leaves
/// nothing mutated.
pub fn admit_composite(
    request: &CompositeAppend,
    mut read_reused: impl FnMut(&Hash) -> Result<Vec<u8>, MediumError>,
) -> Result<CompositeManifest, CompositeAdmissionError> {
    // (1) The strict root decode (the grammar, the version, the
    // family recognition, the bounds, the composed coherence).
    let manifest = composite_manifest::decode(request.root.bytes())
        .map_err(CompositeAdmissionError::Manifest)?;
    // (2) The snapshot binding: the revision references the root.
    let root_identity = request.root.hash();
    if !request.record.content_refs.contains(&root_identity) {
        return Err(CompositeAdmissionError::RootNotReferenced {
            root: root_identity,
        });
    }
    // (3) The exact-set rule: the manifest's declared closure equals
    // the supplied new chunks plus the reused identities — no
    // duplicate, omitted, or extraneous dependency.
    let mut supplied: Vec<Hash> = request.new_chunks.iter().map(StoredObject::hash).collect();
    supplied.extend(request.reused_chunks.iter().copied());
    let supplied_sorted = {
        let mut sorted = supplied.clone();
        sorted.sort();
        sorted
    };
    if supplied_sorted.len() != supplied.len() {
        return Err(CompositeAdmissionError::DependencySet {
            detail: "a duplicate dependency identity in the request".to_owned(),
        });
    }
    let mut declared_sorted = manifest.closure_identities();
    declared_sorted.sort();
    if supplied_sorted != declared_sorted {
        return Err(CompositeAdmissionError::DependencySet {
            detail: "the declared dependency set does not match the supplied new chunks plus the reused identities"
                .to_owned(),
        });
    }
    // (4) EVERY chunk fully verified: the new chunks against their
    // supplied bytes, the reused through the medium.
    for chunk in request.new_chunks {
        verify_chunk_dependency(chunk.bytes(), &chunk.hash(), &manifest)?;
    }
    for identity in request.reused_chunks {
        let bytes = read_reused(identity).map_err(|error| CompositeAdmissionError::Dependency {
            identity: *identity,
            detail: format!("the medium read failed: {error:?}"),
        })?;
        verify_chunk_dependency(&bytes, identity, &manifest)?;
    }
    Ok(manifest)
}

/// The snapshot-family domain gate for the GENERIC append (K2-C1's
/// fail-closed boundary): a supplied object whose leading domain is
/// a session-snapshot family member OTHER than the supported legacy
/// `/1` whole-object form refuses typed — a `/2` composite root (or
/// any unrecognized future family version) can NEVER enter the
/// chain outside the dedicated composite admission, whether newly
/// supplied or re-supplied from storage (the medium's set rule
/// requires every referenced object to be supplied, so the supplied
/// check covers every entry path). The legacy `/1` production path
/// is unchanged.
pub fn enforce_snapshot_family_gate(objects: &[StoredObject]) -> Result<(), MediumError> {
    for object in objects {
        let domain = object.domain();
        if domain.starts_with(composite_manifest::SESSION_SNAPSHOT_FAMILY)
            && domain != composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN
        {
            return Err(MediumError::CompositeDomainGate {
                domain: domain.to_owned(),
            });
        }
    }
    Ok(())
}
