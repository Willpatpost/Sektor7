//! SHA-256 content addressing with domain separation (§5.1).
//!
//! An object's identity is the SHA-256 of its complete canonical bytes,
//! and every canonical object **begins with its length-prefixed domain
//! tag** — so `SHA-256(domain-tag || canonical bytes)` holds
//! structurally, with the tag's length prefix making the concatenation
//! unambiguous (no domain/payload split can alias another).
//!
//! SHA-256 is object **integrity**, never signer authentication — no
//! field in this crate claims who wrote an object, only that its bytes
//! match its hash. FNV-1a remains engine-core's regression instrument
//! and is never substituted for cryptographic integrity here.

use std::fmt;

use sha2::{Digest, Sha256};

/// A 32-byte SHA-256 content hash — an object's identity. Ordered by
/// raw bytes (the canonical collection order for hash lists).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash([u8; 32]);

impl Hash {
    /// The hash width in bytes (a fixed wire width, like u32/u64).
    pub const WIDTH: u64 = 32;

    /// The all-zero hash (a placeholder; never a valid object id in
    /// practice — every real object's hash is its content's).
    pub const ZERO: Hash = Hash([0u8; 32]);

    /// The raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Builds a hash from exactly 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Hash> {
        let array: [u8; 32] = bytes.try_into().ok()?;
        Some(Hash(array))
    }

    /// 64-character lowercase hex — the canonical filename and display
    /// form.
    pub fn hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Parses 64 lowercase-or-uppercase hex characters.
    pub fn from_hex(text: &str) -> Option<Hash> {
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (slot, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            *slot = u8::try_from(high * 16 + low).ok()?;
        }
        Some(Hash(bytes))
    }
}

/// Hashes a complete canonical object. The bytes must begin with the
/// object's length-prefixed domain tag — domain separation is carried
/// by the hashed content itself, not by an out-of-band label.
pub fn hash_object(bytes: &[u8]) -> Hash {
    let mut digest = Sha256::new();
    digest.update(bytes);
    let finalized = digest.finalize();
    Hash(finalized.into())
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.hex())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tagged(domain: &str, payload: &[u8]) -> Vec<u8> {
        // Mirrors the Encoder's length-prefixed string form so the
        // domain-separation property is tested over realistic bytes.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(domain.len() as u64).to_le_bytes());
        bytes.extend_from_slice(domain.as_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn hashing_is_deterministic() {
        let a = hash_object(&tagged("sektor7/test/1", b"payload"));
        let b = hash_object(&tagged("sektor7/test/1", b"payload"));
        assert_eq!(a, b);
    }

    #[test]
    fn different_payloads_hash_differently() {
        let a = hash_object(&tagged("sektor7/test/1", b"payload-a"));
        let b = hash_object(&tagged("sektor7/test/1", b"payload-b"));
        assert_ne!(a, b);
    }

    #[test]
    fn domain_separation_holds_structurally() {
        // The same payload under different domain tags hashes
        // differently — the tag is part of the hashed content and its
        // length prefix prevents split aliasing.
        let a = hash_object(&tagged("sektor7/test/record/1", b"payload"));
        let b = hash_object(&tagged("sektor7/test/snapshot/1", b"payload"));
        assert_ne!(a, b);

        // The length prefix makes "ab" + "c" and "a" + "bc" distinct
        // domains with identical concatenated tails.
        let split = hash_object(&tagged("ab", b"c"));
        let whole = hash_object(&tagged("a", b"bc"));
        assert_ne!(split, whole);
    }

    #[test]
    fn hex_round_trips() {
        let hash = hash_object(&tagged("sektor7/test/1", b"payload"));
        let text = hash.hex();
        assert_eq!(text.len(), 64);
        assert_eq!(Hash::from_hex(&text), Some(hash));
    }

    #[test]
    fn from_hex_rejects_malformed_input() {
        assert_eq!(Hash::from_hex(""), None);
        assert_eq!(Hash::from_hex("zz"), None);
        assert_eq!(
            Hash::from_hex(&"0".repeat(63)),
            None,
            "63 characters is not a hash"
        );
        assert_eq!(
            Hash::from_hex(&"g".repeat(64)),
            None,
            "non-hex characters are rejected"
        );
    }
}
