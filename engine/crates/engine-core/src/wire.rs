//! Canonical fixed-width wire format for persistent engine state
//! (Stage 5 Batch 1 — `docs/STAGE5-B1-WIRE-FORMAT.md` is the frozen
//! specification).
//!
//! The discipline mirrors the Fingerprinter's: little-endian fixed-width
//! integers, u64-length-prefixed byte arrays and strings, no `usize` and
//! no native-memory layout anywhere on the wire — bytes that are
//! identical across platforms, compilers, and processes. Section
//! boundaries carry versioned domain tags so a byte stream's schema is
//! unambiguous.
//!
//! Decoding is fail-closed with typed errors: truncation, unsupported
//! format versions, wrong domain tags, invalid enum discriminants,
//! invalid UTF-8, lengths or counts beyond their documented maxima
//! (checked *before* any allocation trusts them — a corrupted length
//! field is never used to reserve memory), noncanonical collection
//! order, duplicate identities, checked-arithmetic overflow, and
//! unexpected trailing bytes are all rejected. Malformed input is never
//! repaired and never produces a success-shaped value.

use std::fmt;

use crate::clock::Tick;
use crate::spine::World;

/// The save format version this build writes and understands
/// (wire-format spec §versioning). Readers reject every other value —
/// they never guess across versions; migration is a ratified future
/// mechanism, never silent.
pub const SAVE_FORMAT_VERSION: u32 = 1;

/// Domain tag of a complete save's header section (wire-format spec
/// §sections): the outermost schema identity of the byte stream.
pub const SAVE_DOMAIN: &str = "sektor7/save/1";

/// Domain tag of the spine section (wire-format spec §sections).
pub const SPINE_DOMAIN: &str = "engine-core/spine-save/1";

/// Documented maximum for a length-prefixed byte array or string
/// (wire-format spec §maxima). Values above are rejected before any
/// allocation.
pub const MAX_BYTE_LEN: u64 = 1 << 32;

/// Documented maximum for a collection count (wire-format spec
/// §maxima), additionally bounded in every section reader by the bytes
/// actually remaining in the input — no element is smaller than one
/// byte, so a count above the remainder is corruption, never a
/// reservation.
pub const MAX_COLLECTION_COUNT: u64 = 1 << 32;

/// Documented maximum for a domain-tag string.
pub const MAX_TAG_LEN: u64 = 1 << 16;

/// Save/decode failure. Every variant names a documented wire-format
/// violation; decoding is fail-closed — none is silently repaired and
/// none yields a success-shaped value for corrupted state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The input ended before a complete value was present.
    Truncated,
    /// Bytes remained after the final section of a complete save.
    Trailing { leftover: u64 },
    /// A section's domain tag did not match the expected tag
    /// (`expected` names the section's schema; `observed` is what the
    /// bytes carried).
    Domain {
        expected: &'static str,
        observed: String,
    },
    /// The save format version is not supported by this build; both
    /// versions are named — readers never guess across versions.
    Version { observed: u32, supported: u32 },
    /// An enum discriminant is not a defined value.
    Enum {
        section: &'static str,
        discriminant: u64,
    },
    /// A string field was not valid UTF-8.
    Utf8,
    /// A length or count exceeded a documented maximum, or exceeded
    /// the bytes remaining in the input — rejected *before* any
    /// allocation trusts the value.
    Limit {
        section: &'static str,
        kind: &'static str,
        value: u64,
        max: u64,
    },
    /// Checked arithmetic on an integrity-gate sum overflowed — the
    /// counters are not representable, so the save cannot be sound.
    Overflow { section: &'static str },
    /// A canonically ordered collection section was out of order.
    Order { section: &'static str },
    /// A duplicate identity within a collection section.
    Duplicate { section: &'static str, seq: u64 },
    /// A load-time integrity gate failed; the name identifies the gate
    /// (wire-format spec §integrity).
    Integrity(&'static str),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Truncated => write!(f, "truncated save input"),
            SaveError::Trailing { leftover } => {
                write!(
                    f,
                    "{leftover} trailing byte(s) after the save's final section"
                )
            }
            SaveError::Domain { expected, observed } => {
                write!(
                    f,
                    "domain tag mismatch: expected {expected:?}, observed {observed:?}"
                )
            }
            SaveError::Version {
                observed,
                supported,
            } => write!(
                f,
                "unsupported save format version {observed} (this build supports {supported})"
            ),
            SaveError::Enum {
                section,
                discriminant,
            } => {
                write!(f, "invalid enum discriminant {discriminant} in {section}")
            }
            SaveError::Utf8 => write!(f, "string field is not valid UTF-8"),
            SaveError::Limit {
                section,
                kind,
                value,
                max,
            } => write!(f, "{kind} {value} in {section} exceeds its maximum {max}"),
            SaveError::Overflow { section } => {
                write!(f, "checked arithmetic overflow in {section}")
            }
            SaveError::Order { section } => {
                write!(f, "noncanonical collection order in {section}")
            }
            SaveError::Duplicate { section, seq } => {
                write!(f, "duplicate identity #{seq} in {section}")
            }
            SaveError::Integrity(gate) => write!(f, "integrity gate failed: {gate}"),
        }
    }
}

impl std::error::Error for SaveError {}

/// Canonical byte producer (wire-format spec §primitives): appends
/// little-endian fixed-width values and u64-length-prefixed byte
/// arrays and strings. No `usize` reaches the wire without a checked
/// conversion to the u64 width.
#[derive(Debug, Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Encoder { buf: Vec::new() }
    }

    /// The encoded bytes so far.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consumes the encoder, returning the canonical bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn write_u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub fn write_u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_tick(&mut self, tick: Tick) {
        self.write_u64(tick.as_u64());
    }

    /// Writes a collection count or length prefix. `usize` fits `u64`
    /// on every supported target; the conversion is checked so a
    /// hypothetical wider target fails loudly instead of truncating
    /// silently.
    pub fn write_count(&mut self, count: usize) {
        let wide = u64::try_from(count).expect("usize exceeds the u64 wire width");
        self.write_u64(wide);
    }

    /// Writes a u64-length-prefixed byte array.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_count(bytes.len());
        self.buf.extend_from_slice(bytes);
    }

    /// Writes a u64-length-prefixed UTF-8 string — the Fingerprinter's
    /// length-prefix discipline: embedded NULs are safe and no split
    /// aliases a concatenation.
    pub fn write_str(&mut self, text: &str) {
        self.write_bytes(text.as_bytes());
    }
}

/// Strict canonical byte consumer (wire-format spec §primitives).
/// Every read is bounds-checked, and every length or count is
/// validated against its documented maximum *and* the bytes actually
/// remaining before any caller may allocate from it.
#[derive(Debug)]
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Decoder { buf, pos: 0 }
    }

    /// Bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, want: usize) -> Result<&'a [u8], SaveError> {
        let end = self.pos.checked_add(want).ok_or(SaveError::Truncated)?;
        if end > self.buf.len() {
            return Err(SaveError::Truncated);
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub fn read_u8(&mut self) -> Result<u8, SaveError> {
        Ok(self.take(1)?[0])
    }

    pub fn read_u32(&mut self) -> Result<u32, SaveError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_u64(&mut self) -> Result<u64, SaveError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub fn read_tick(&mut self) -> Result<Tick, SaveError> {
        Ok(Tick(self.read_u64()?))
    }

    /// Reads a length/count prefix and bounds it before any allocation
    /// may trust it: the documented `max` for the section, and the
    /// bytes actually remaining (no element is smaller than one byte).
    /// Corrupted length fields are rejected, never reserved.
    pub fn read_count(
        &mut self,
        max: u64,
        section: &'static str,
        kind: &'static str,
    ) -> Result<u64, SaveError> {
        let value = self.read_u64()?;
        if value > max {
            return Err(SaveError::Limit {
                section,
                kind,
                value,
                max,
            });
        }
        let remaining = self.remaining() as u64;
        if value > remaining {
            return Err(SaveError::Limit {
                section,
                kind,
                value,
                max: remaining,
            });
        }
        Ok(value)
    }

    /// Reads a u64-length-prefixed byte array, bounded by `max` and
    /// the remaining input.
    pub fn read_bytes(&mut self, max: u64, section: &'static str) -> Result<&'a [u8], SaveError> {
        let len = self.read_count(max, section, "byte-length")?;
        // Bounded by `remaining` above, so the cast cannot truncate.
        self.take(len as usize)
    }

    /// Reads a u64-length-prefixed UTF-8 string, bounded by `max`.
    pub fn read_str(&mut self, max: u64, section: &'static str) -> Result<&'a str, SaveError> {
        let bytes = self.read_bytes(max, section)?;
        std::str::from_utf8(bytes).map_err(|_| SaveError::Utf8)
    }

    /// Reads a section's domain tag and requires an exact match with
    /// `expected` — the schema/domain gate (wire-format spec
    /// §domain-separation).
    pub fn read_domain(&mut self, expected: &'static str) -> Result<(), SaveError> {
        let observed = self.read_str(MAX_TAG_LEN, "domain-tag")?;
        if observed != expected {
            return Err(SaveError::Domain {
                expected,
                observed: observed.to_owned(),
            });
        }
        Ok(())
    }

    /// End of a save: no bytes may remain (wire-format spec
    /// §trailing). Unconsumed trailing data is corruption, not noise.
    pub fn finish(&mut self) -> Result<(), SaveError> {
        if self.pos != self.buf.len() {
            return Err(SaveError::Trailing {
                leftover: (self.buf.len() - self.pos) as u64,
            });
        }
        Ok(())
    }
}

/// Generic world codec boundary (Stage 5 Batch 1): how an application
/// [`World`] serializes and restores itself as the world section of a
/// spine save. Engine-core owns the mechanism — the wire, the section
/// framing, and the integrity gates — while the application owns its
/// own state representation (the `NpcWorld`/`RegionDirector`
/// integration arrives in Batch 4; Batch 1 proves the boundary with
/// focused test worlds).
///
/// Implementations encode canonical bytes only (fixed-width,
/// deterministic, canonically ordered collections — this module's
/// discipline) and restore fail-closed: [`Self::restore`] must consume
/// exactly its own encoding or return a typed [`SaveError`]; malformed
/// input is never repaired.
pub trait SaveWorld: World {
    /// The world section's domain tag — versioned and stable, distinct
    /// from every other section's tag (wire-format spec
    /// §domain-separation).
    const SAVE_DOMAIN: &'static str;

    /// Encodes the world's authoritative state.
    fn save(&self, enc: &mut Encoder);

    /// Restores a world from its section bytes.
    fn restore(dec: &mut Decoder) -> Result<Self, SaveError>
    where
        Self: Sized;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_u8(0xab);
        enc.write_u32(0x1234_5678);
        enc.write_u64(0x0102_0304_0506_0708);
        enc.write_tick(Tick(42));
        enc.write_bytes(&[1, 2, 3]);
        enc.write_str("sektor7");
        enc.into_bytes()
    }

    #[test]
    fn primitives_round_trip() {
        let bytes = sample();
        let mut dec = Decoder::new(&bytes);
        assert_eq!(dec.read_u8().unwrap(), 0xab);
        assert_eq!(dec.read_u32().unwrap(), 0x1234_5678);
        assert_eq!(dec.read_u64().unwrap(), 0x0102_0304_0506_0708);
        assert_eq!(dec.read_tick().unwrap(), Tick(42));
        assert_eq!(dec.read_bytes(MAX_BYTE_LEN, "test").unwrap(), &[1, 2, 3]);
        assert_eq!(dec.read_str(MAX_BYTE_LEN, "test").unwrap(), "sektor7");
        dec.finish().unwrap();
    }

    #[test]
    fn byte_order_is_little_endian() {
        let mut enc = Encoder::new();
        enc.write_u32(1);
        assert_eq!(enc.into_bytes(), [1, 0, 0, 0]);

        let mut enc = Encoder::new();
        enc.write_u64(1);
        assert_eq!(enc.into_bytes(), [1, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn encoder_is_deterministic() {
        let mut a = Encoder::new();
        a.write_str("sektor7/save");
        a.write_u64(7);
        let mut b = Encoder::new();
        b.write_str("sektor7/save");
        b.write_u64(7);
        assert_eq!(a.into_bytes(), b.into_bytes());
    }

    #[test]
    fn length_prefixes_separate_splits() {
        // The Fingerprinter discipline carried onto the wire: no
        // string split may alias a concatenation.
        let mut whole = Encoder::new();
        whole.write_str("ab");
        whole.write_str("c");
        let mut split = Encoder::new();
        split.write_str("a");
        split.write_str("bc");
        assert_ne!(whole.into_bytes(), split.into_bytes());
    }

    #[test]
    fn truncation_rejected_at_every_boundary() {
        let bytes = sample();
        for cut in 0..bytes.len() {
            let mut dec = Decoder::new(&bytes[..cut]);
            let walked = dec
                .read_u8()
                .and_then(|_| dec.read_u32())
                .and_then(|_| dec.read_u64())
                .and_then(|_| dec.read_tick())
                .and_then(|_| dec.read_bytes(MAX_BYTE_LEN, "test"))
                .and_then(|_| dec.read_str(MAX_BYTE_LEN, "test"))
                .and_then(|_| dec.finish());
            assert!(
                walked.is_err(),
                "a prefix of {cut} bytes must not parse as a complete value stream"
            );
        }
        // The full buffer parses and finishes cleanly.
        let mut dec = Decoder::new(&bytes);
        assert!(
            dec.read_u8()
                .and_then(|_| dec.read_u32())
                .and_then(|_| dec.read_u64())
                .and_then(|_| dec.read_tick())
                .and_then(|_| dec.read_bytes(MAX_BYTE_LEN, "test"))
                .and_then(|_| dec.read_str(MAX_BYTE_LEN, "test"))
                .and_then(|_| dec.finish())
                .is_ok()
        );
    }

    #[test]
    fn count_beyond_remaining_is_rejected_before_allocation() {
        let mut enc = Encoder::new();
        enc.write_count(5);
        enc.write_u8(1);
        enc.write_u8(2);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        let error = dec
            .read_count(MAX_COLLECTION_COUNT, "test", "entry-count")
            .unwrap_err();
        assert!(matches!(error, SaveError::Limit { value: 5, .. }));
    }

    #[test]
    fn count_beyond_documented_maximum_is_rejected() {
        let mut enc = Encoder::new();
        enc.write_u64(u64::MAX);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        let error = dec
            .read_count(MAX_COLLECTION_COUNT, "test", "entry-count")
            .unwrap_err();
        assert!(matches!(
            error,
            SaveError::Limit {
                value: u64::MAX,
                max: MAX_COLLECTION_COUNT,
                ..
            }
        ));
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let mut enc = Encoder::new();
        enc.write_count(2);
        enc.write_u8(0xff);
        enc.write_u8(0xfe);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert_eq!(
            dec.read_str(MAX_BYTE_LEN, "test").unwrap_err(),
            SaveError::Utf8
        );
    }

    #[test]
    fn domain_mismatch_names_both_tags() {
        let mut enc = Encoder::new();
        enc.write_str("a/b/1");
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(matches!(
            dec.read_domain("x/y/1"),
            Err(SaveError::Domain { .. })
        ));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = sample();
        bytes.push(0);
        let mut dec = Decoder::new(&bytes);
        dec.read_u8().unwrap();
        dec.read_u32().unwrap();
        dec.read_u64().unwrap();
        dec.read_tick().unwrap();
        dec.read_bytes(MAX_BYTE_LEN, "test").unwrap();
        dec.read_str(MAX_BYTE_LEN, "test").unwrap();
        assert!(matches!(
            dec.finish(),
            Err(SaveError::Trailing { leftover: 1 })
        ));
    }
}
