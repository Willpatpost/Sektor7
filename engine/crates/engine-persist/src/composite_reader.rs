//! The S6-C4 Phase 2 K2-B DARK DEPENDENCY-CLOSURE READER — the
//! non-publishing, non-destructive READ side of the ratified
//! `engine-slice/session-snapshot/2` composite manifest (the K2-A
//! schema). **Dark by construction:** no production read, write, or
//! reclamation path depends on this module; no provider implements
//! [`ObjectSource`] outside of tests; nothing here mutates storage —
//! the reader only reads, hash-verifies, and reconstructs. The
//! future reader-side closure (the K2-C checkpoint, should it be
//! authorized) will bind this contract to the real mediums; until
//! then the module is exercised exclusively by tests.
//!
//! **The contract.** Given a VERIFIED root object (the caller read
//! it and its bytes hash to its identity — the reader re-derives
//! the identity itself, never trusting a supplied hash), the
//! accepted K2-A manifest schema, a read-only object source, and
//! EXPLICIT resource limits, [`read_composite_closure`] proves that
//! the manifest's ENTIRE dependency closure exists, matches its
//! authenticated identities, is well formed, and reconstructs the
//! composed legacy payload stream:
//!
//! - The root strictly decodes through the ratified schema (the
//!   grammar, the cardinalities, the run rules, the bounds — the
//!   K2-A decoder, unchanged).
//! - EVERY declared chunk is loaded through the source and
//!   hash-verified BY THE READER (the envelope re-hash — the
//!   reader's own verification, independent of whatever the source
//!   checked): the chunk envelope's domain, section tag, declared
//!   payload length, and actual payload bytes must all match the
//!   manifest's entry, and the whole object's SHA-256 must equal
//!   the declared identity.
//! - Missing or corrupt dependencies fail CLOSED with their
//!   identities preserved ([`CompositeReadError::Missing`],
//!   [`CompositeReadError::HashMismatch`], the typed envelope
//!   mismatches) — no partial successful result is ever returned
//!   when a required dependency is unresolved: the limit checks run
//!   before the first source read, and any per-chunk failure
//!   aborts the whole operation.
//! - The verified chunks return IN THE MANIFEST'S FIXED ORDER with
//!   their identities — the distinct dependency set (the manifest
//!   decode's duplicate rejection makes the entry identities a set)
//!   suitable for future canonical reachability and sweeping.
//!
//! **The legacy framing is engine-slice's, not this crate's** (the
//! dependency direction is ratified: engine-persist never depends on
//! engine-slice). The reader returns the ordered section payloads —
//! an equivalent bounded reconstruction; `engine-slice`'s dark
//! integration applies the legacy `session-snapshot/1` framing to
//! produce the exact legacy bytes and passes them through the
//! existing strict `restore_session()` contract — the sole
//! authority over global session validity. Hash-valid chunks are
//! NEVER equated with a globally valid reconstructed session.
//!
//! **The verification memo** ([`VerificationMemo`]): within ONE
//! bounded verification operation the caller may thread one memo
//! across multiple reads; an identity is memoized only AFTER its
//! complete verified read (the envelope, the tag, the length, and
//! the hash all checked), and a memoized chunk is STILL checked
//! against the new manifest's entry declaration — the memo skips
//! the re-read and re-hash, never the coherence. The assumptions
//! under which an immutable object is reused safely within one
//! operation: the source is content-addressed (the identity IS the
//! SHA-256 of the bytes — a source returning different bytes for
//! the same identity within one operation is corrupt by
//! definition), and the operation is single-threaded and bounded by
//! the caller. No persistent trust across independent openings (a
//! memo is created per operation and dropped with it), no trust
//! based solely on a supplied hash (the memo's entries are EARNED
//! by full verification, never supplied), and deterministic
//! behavior with the memo enabled or disabled (a fresh memo per
//! read is exactly the no-memo behavior — the manifest decode
//! forbids duplicate identities within one manifest, so the memo
//! only fires across manifests sharing chunks). No production
//! reuse-verification cache exists or is introduced.

use std::collections::HashMap;

use engine_core::wire::MAX_TAG_LEN;
use engine_core::{Decoder, SaveError};

use crate::StoredObject;
use crate::composite_manifest::{
    self, CompositeManifest, MAX_CHUNK_BYTES, MAX_COMPOSED_LENGTH, MAX_MANIFEST_CHUNKS,
    SESSION_CHUNK_DOMAIN,
};
use crate::hash::{Hash, hash_object};

// ---------------------------------------------------- the read-only source

/// The reader's only storage dependency: a minimal, READ-ONLY,
/// content-addressed byte source. `&mut self` mirrors
/// [`crate::CanonicalMedium::read_object`] so a future verified
/// bridge is a pure delegation — the trait's only operations are
/// reads. Implementations: test fixtures, isolated mock stores, and
/// (NOT in this checkpoint) read-only views over the real mediums.
pub trait ObjectSource {
    /// Reads one object's canonical bytes by content identity.
    /// `Missing` when the identity is absent; `Unavailable` for any
    /// other retrieval failure (the reader maps both to typed
    /// fail-closed errors carrying the identity).
    fn read_object(&mut self, identity: &Hash) -> Result<Vec<u8>, ObjectSourceError>;
}

/// A source retrieval failure, distinguished by what the reader can
/// prove: absence (`Missing` — the dependency is unresolved) versus
/// an unavailable source (transport/io — nothing is concluded about
/// the object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectSourceError {
    Missing(Hash),
    Unavailable(String),
}

// ------------------------------------------------------------- the limits

/// The reader's explicit decoding and resource limits — a
/// per-operation TIGHTENING of the schema's own maxima (a limit
/// above the schema's bound is a caller misconfiguration and fails
/// typed, never silently clamped). Checked BEFORE any source read:
/// a manifest the limits refuse costs zero object reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    /// The maximum manifest entry count (≤ the schema's 4096).
    pub max_chunks: u64,
    /// The maximum per-chunk payload length (≤ the schema's 8 MiB).
    pub max_chunk_bytes: u64,
    /// The maximum composed payload total (≤ the schema's 1 GiB).
    pub max_composed_length: u64,
}

impl Default for ReadLimits {
    fn default() -> Self {
        ReadLimits {
            max_chunks: MAX_MANIFEST_CHUNKS,
            max_chunk_bytes: MAX_CHUNK_BYTES,
            max_composed_length: MAX_COMPOSED_LENGTH,
        }
    }
}

impl ReadLimits {
    fn validate(&self) -> Result<(), CompositeReadError> {
        if self.max_chunks > MAX_MANIFEST_CHUNKS {
            return Err(CompositeReadError::Limit {
                kind: "read-limits/chunk-count",
                value: self.max_chunks,
                max: MAX_MANIFEST_CHUNKS,
            });
        }
        if self.max_chunk_bytes > MAX_CHUNK_BYTES {
            return Err(CompositeReadError::Limit {
                kind: "read-limits/chunk-length",
                value: self.max_chunk_bytes,
                max: MAX_CHUNK_BYTES,
            });
        }
        if self.max_composed_length > MAX_COMPOSED_LENGTH {
            return Err(CompositeReadError::Limit {
                kind: "read-limits/composed-length",
                value: self.max_composed_length,
                max: MAX_COMPOSED_LENGTH,
            });
        }
        Ok(())
    }
}

// --------------------------------------------------------------- the memo

/// The per-operation verification memo — see the module
/// documentation for the reuse assumptions. An entry exists only
/// AFTER a complete verified read of the exact object; a memoized
/// chunk is still checked against every new manifest's entry
/// declaration. Created per bounded operation; no static state, no
/// persistence, no trust in supplied hashes.
#[derive(Debug, Default)]
pub struct VerificationMemo {
    entries: HashMap<Hash, MemoEntry>,
}

#[derive(Debug, Clone)]
struct MemoEntry {
    /// The section tag the first full verification established
    /// (the envelope's observed tag, proven equal to the declaring
    /// manifest entry's tag).
    tag: &'static str,
    payload: Vec<u8>,
}

impl VerificationMemo {
    /// A fresh memo for one bounded verification operation.
    pub fn new() -> Self {
        VerificationMemo {
            entries: HashMap::new(),
        }
    }

    /// The number of fully verified identities memoized so far (the
    /// memo's accounting witness — the verification-cost model's
    /// distinct-versus-repeated measurement).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been memoized yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether this identity has been FULLY verified within this
    /// operation (read-only introspection for the memo-correctness
    /// proofs — presence means an earned entry, never a supplied
    /// hash).
    pub fn contains(&self, identity: &Hash) -> bool {
        self.entries.contains_key(identity)
    }
}

// -------------------------------------------------------- the read result

/// One fully verified dependency chunk: the manifest entry's
/// section tag, its authenticated identity, and its verified
/// payload (an unmodified slice of the exact legacy serialization).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedChunk {
    pub tag: &'static str,
    pub identity: Hash,
    pub payload: Vec<u8>,
}

/// The complete verified dependency closure of one composite root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedComposite {
    /// The strictly decoded root manifest (the K2-A decoder's full
    /// gate set already applied).
    pub manifest: CompositeManifest,
    /// The root's content identity — re-derived by the reader from
    /// the supplied root bytes (never a supplied hash).
    pub root_identity: Hash,
    /// The verified chunks in the manifest's fixed order (the
    /// grammar's order — the legacy composition order).
    pub chunks: Vec<VerifiedChunk>,
}

impl VerifiedComposite {
    /// The verified identities in the manifest's fixed order.
    pub fn verified_identities(&self) -> Vec<Hash> {
        self.chunks.iter().map(|chunk| chunk.identity).collect()
    }

    /// The DISTINCT dependency identities — the reachability
    /// accounting surface. Distinct by the manifest decode's
    /// duplicate-identity rejection: the entry identities are a
    /// set, so the ordered list IS the distinct set.
    pub fn dependency_identities(&self) -> Vec<Hash> {
        self.verified_identities()
    }

    /// The composed payload stream — the chunks' payloads
    /// concatenated in the grammar's fixed order (bounded by the
    /// manifest's verified composed length). The legacy
    /// `session-snapshot/1` framing (the domain tag and the two
    /// blob length prefixes) is engine-slice's to apply.
    pub fn composed_payloads(&self) -> Vec<u8> {
        let mut composed = Vec::new();
        for chunk in &self.chunks {
            composed.extend_from_slice(&chunk.payload);
        }
        composed
    }
}

// --------------------------------------------------------------- the errors

/// Every reader failure is typed and identity-preserving: a missing
/// or corrupt dependency reports WHICH identity failed and what was
/// proven about it. No panic, no partial success.
#[derive(Debug, Clone, PartialEq)]
pub enum CompositeReadError {
    /// The root manifest failed the strict ratified decode (the
    /// grammar, the version, the domain family, the bounds, the
    /// coherence).
    Manifest(SaveError),
    /// The source reports the identity absent — the dependency is
    /// unresolved.
    Missing { identity: Hash },
    /// The source failed for a non-absence reason — nothing is
    /// concluded about the object; the operation fails closed.
    Source { identity: Hash, detail: String },
    /// The object's envelope does not decode as
    /// `domain + tag + length-prefixed payload` with no trailing
    /// bytes (truncated, malformed, or trailing content).
    Envelope { identity: Hash, error: SaveError },
    /// The envelope's leading domain is not the ratified chunk
    /// domain.
    Domain { identity: Hash, observed: String },
    /// The envelope's section tag does not match the manifest
    /// entry's declaration (checked before the hash so a
    /// mis-tagged object reports the tag, not merely a hash
    /// mismatch).
    Tag {
        identity: Hash,
        expected: &'static str,
        observed: String,
    },
    /// The manifest entry's declared payload length does not match
    /// the envelope's actual payload length.
    Length {
        identity: Hash,
        declared: u64,
        actual: u64,
    },
    /// The object's bytes do not SHA-256 to the declared identity —
    /// the reader's own re-verification, independent of the source.
    HashMismatch { identity: Hash },
    /// A resource limit refused the manifest before any source read
    /// (the caller's tightening, or a misconfigured limit above the
    /// schema's bound).
    Limit {
        kind: &'static str,
        value: u64,
        max: u64,
    },
}

impl std::fmt::Display for CompositeReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompositeReadError::Manifest(error) => {
                write!(f, "the root manifest failed the strict decode: {error}")
            }
            CompositeReadError::Missing { identity } => {
                write!(f, "the dependency {} is missing", identity.hex())
            }
            CompositeReadError::Source { identity, detail } => {
                write!(
                    f,
                    "the source failed for {} without proving absence: {detail}",
                    identity.hex()
                )
            }
            CompositeReadError::Envelope { identity, error } => {
                write!(
                    f,
                    "the chunk {} envelope is malformed: {error}",
                    identity.hex()
                )
            }
            CompositeReadError::Domain { identity, observed } => {
                write!(
                    f,
                    "the chunk {} envelope carries the foreign domain {observed}",
                    identity.hex()
                )
            }
            CompositeReadError::Tag {
                identity,
                expected,
                observed,
            } => {
                write!(
                    f,
                    "the chunk {} is tagged {observed}, the manifest declares {expected}",
                    identity.hex()
                )
            }
            CompositeReadError::Length {
                identity,
                declared,
                actual,
            } => {
                write!(
                    f,
                    "the chunk {} declares {declared} payload bytes, the envelope carries {actual}",
                    identity.hex()
                )
            }
            CompositeReadError::HashMismatch { identity } => {
                write!(
                    f,
                    "the chunk {} bytes do not hash to the declared identity",
                    identity.hex()
                )
            }
            CompositeReadError::Limit { kind, value, max } => {
                write!(f, "the limit {kind} refuses the value {value} (max {max})")
            }
        }
    }
}

impl std::error::Error for CompositeReadError {}

// ---------------------------------------------------------------- the read

/// Reads and fully verifies one composite root's dependency closure
/// with a fresh per-call memo (exactly the no-memoization behavior
/// — a manifest's identities are distinct, so nothing repeats
/// within one manifest). See [`read_composite_closure_with_memo`].
pub fn read_composite_closure(
    source: &mut dyn ObjectSource,
    root: &StoredObject,
    limits: &ReadLimits,
) -> Result<VerifiedComposite, CompositeReadError> {
    let mut memo = VerificationMemo::new();
    read_composite_closure_with_memo(source, root, limits, &mut memo)
}

/// Reads and fully verifies one composite root's dependency closure,
/// threading the caller's per-operation [`VerificationMemo`] —
/// chunks already fully verified within THIS operation (an earlier
/// root in the same verification pass) skip the re-read and re-hash
/// under the documented content-addressing assumptions, while every
/// entry-level coherence check (the tag, the declared length) still
/// runs against THIS manifest's declaration. Deterministic with or
/// without the memo.
pub fn read_composite_closure_with_memo(
    source: &mut dyn ObjectSource,
    root: &StoredObject,
    limits: &ReadLimits,
    memo: &mut VerificationMemo,
) -> Result<VerifiedComposite, CompositeReadError> {
    limits.validate()?;
    // The strict ratified decode — the grammar, the cardinalities,
    // the run rules, the schema's own bounds, the composed-length
    // coherence.
    let manifest =
        composite_manifest::decode(root.bytes()).map_err(CompositeReadError::Manifest)?;
    // The caller's resource limits, ALL checked before the first
    // source read — a refused manifest costs zero object reads and
    // can never produce a partial result.
    if manifest.entries.len() as u64 > limits.max_chunks {
        return Err(CompositeReadError::Limit {
            kind: "read/chunk-count",
            value: manifest.entries.len() as u64,
            max: limits.max_chunks,
        });
    }
    if manifest.composed_length > limits.max_composed_length {
        return Err(CompositeReadError::Limit {
            kind: "read/composed-length",
            value: manifest.composed_length,
            max: limits.max_composed_length,
        });
    }
    for entry in &manifest.entries {
        if entry.payload_length > limits.max_chunk_bytes {
            return Err(CompositeReadError::Limit {
                kind: "read/chunk-length",
                value: entry.payload_length,
                max: limits.max_chunk_bytes,
            });
        }
    }
    let mut chunks = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        // The memoized path: the identity was FULLY verified within
        // this operation (the envelope, the tag, the length, and
        // the hash) — reuse the verified payload under the
        // documented immutability assumption. The entry-level
        // coherence checks below still run: a memoized chunk is
        // never blanket-accepted against a new declaration.
        let (observed_tag, payload) = if let Some(memoized) = memo.entries.get(&entry.identity) {
            (memoized.tag.to_owned(), memoized.payload.clone())
        } else {
            // The fresh path: load, parse, and verify the exact
            // object. Every failure is typed with the identity.
            let bytes = source
                .read_object(&entry.identity)
                .map_err(|error| match error {
                    ObjectSourceError::Missing(identity) => {
                        CompositeReadError::Missing { identity }
                    }
                    ObjectSourceError::Unavailable(detail) => CompositeReadError::Source {
                        identity: entry.identity,
                        detail,
                    },
                })?;
            let mut dec = Decoder::new(&bytes);
            let observed_domain =
                dec.read_str(MAX_TAG_LEN, "k2b-chunk-envelope")
                    .map_err(|error| CompositeReadError::Envelope {
                        identity: entry.identity,
                        error,
                    })?;
            if observed_domain != SESSION_CHUNK_DOMAIN {
                return Err(CompositeReadError::Domain {
                    identity: entry.identity,
                    observed: observed_domain.to_owned(),
                });
            }
            let observed_tag =
                dec.read_str(MAX_TAG_LEN, "k2b-chunk-envelope")
                    .map_err(|error| CompositeReadError::Envelope {
                        identity: entry.identity,
                        error,
                    })?;
            let payload = dec
                .read_bytes(limits.max_chunk_bytes, "k2b-chunk-envelope")
                .map_err(|error| CompositeReadError::Envelope {
                    identity: entry.identity,
                    error,
                })?;
            dec.finish().map_err(|error| CompositeReadError::Envelope {
                identity: entry.identity,
                error,
            })?;
            // The entry-level coherence BEFORE the hash — a
            // mis-tagged or mis-lengthed object reports the
            // specific mismatch, not merely a hash failure (the
            // common section below re-checks for the memoized
            // path; for the fresh path the re-check is an
            // idempotent comparison).
            if observed_tag != entry.tag {
                return Err(CompositeReadError::Tag {
                    identity: entry.identity,
                    expected: entry.tag,
                    observed: observed_tag.to_owned(),
                });
            }
            if payload.len() as u64 != entry.payload_length {
                return Err(CompositeReadError::Length {
                    identity: entry.identity,
                    declared: entry.payload_length,
                    actual: payload.len() as u64,
                });
            }
            // The identity re-verification — the whole object's
            // SHA-256 must equal the declared identity, whatever
            // the source checked.
            if hash_object(&bytes) != entry.identity {
                return Err(CompositeReadError::HashMismatch {
                    identity: entry.identity,
                });
            }
            let observed_tag = observed_tag.to_owned();
            let payload = payload.to_vec();
            // The complete verified read EARNS the memo entry —
            // only now may a later manifest in this operation reuse
            // the identity.
            memo.entries.insert(
                entry.identity,
                MemoEntry {
                    tag: entry.tag,
                    payload: payload.clone(),
                },
            );
            (observed_tag, payload)
        };
        // THE entry-level coherence (both paths — the tag and the
        // declared length are checked against THIS manifest's
        // declaration, memoized or fresh): a mis-tagged or
        // mis-lengthed object reports the specific mismatch, never
        // a blanket acceptance.
        if observed_tag != entry.tag {
            return Err(CompositeReadError::Tag {
                identity: entry.identity,
                expected: entry.tag,
                observed: observed_tag,
            });
        }
        if payload.len() as u64 != entry.payload_length {
            return Err(CompositeReadError::Length {
                identity: entry.identity,
                declared: entry.payload_length,
                actual: payload.len() as u64,
            });
        }
        chunks.push(VerifiedChunk {
            tag: entry.tag,
            identity: entry.identity,
            payload,
        });
    }
    Ok(VerifiedComposite {
        manifest,
        root_identity: root.hash(),
        chunks,
    })
}
