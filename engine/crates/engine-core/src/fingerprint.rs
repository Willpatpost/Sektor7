//! Deterministic logical-state fingerprints (FNV-1a, 64-bit).
//!
//! The engine's fingerprints never use `std`'s hashers: `DefaultHasher` is
//! explicitly unstable across versions and platforms. FNV-1a is tiny, fully
//! specified, and stable everywhere.
//!
//! Fingerprint rule (P4 — determinism by construction): never feed data that
//! derives from unordered iteration (e.g. `HashMap` traversal). Engine state
//! uses ordered collections only; queued events are sorted before hashing.

/// FNV-1a 64-bit fingerprint builder.
#[derive(Debug, Clone)]
#[must_use = "a fingerprinter that is never finished proves nothing"]
pub struct Fingerprinter {
    state: u64,
}

impl Default for Fingerprinter {
    fn default() -> Self {
        Self::new()
    }
}

impl Fingerprinter {
    /// FNV-1a 64-bit offset basis.
    pub const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    /// FNV-1a 64-bit prime.
    pub const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn new() -> Self {
        Fingerprinter {
            state: Self::OFFSET_BASIS,
        }
    }

    pub fn write_u8(&mut self, byte: u8) {
        self.state ^= u64::from(byte);
        self.state = self.state.wrapping_mul(Self::PRIME);
    }

    pub fn write_u32(&mut self, value: u32) {
        for byte in value.to_le_bytes() {
            self.write_u8(byte);
        }
    }

    pub fn write_u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.write_u8(byte);
        }
    }

    pub fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u8(byte);
        }
    }

    /// Writes a NUL-terminated string so `("ab", "c")` and `("a", "bc")`
    /// fingerprint differently.
    pub fn write_str(&mut self, text: &str) {
        self.write_bytes(text.as_bytes());
        self.write_u8(0);
    }

    pub fn write_tick(&mut self, tick: crate::Tick) {
        self.write_u64(tick.as_u64());
    }

    pub fn finish(&self) -> u64 {
        self.state
    }

    /// 16-character lowercase hex form (the project's canonical display form).
    pub fn finish_hex(&self) -> String {
        format!("{:016x}", self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_reference_vectors() {
        // Published FNV-1a 64-bit test vectors (draft-eastlake-fnv).
        let empty = Fingerprinter::new();
        assert_eq!(empty.finish(), 0xcbf2_9ce4_8422_2325);

        let mut a = Fingerprinter::new();
        a.write_bytes(b"a");
        assert_eq!(a.finish(), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn write_str_terminator_separates_splits() {
        let mut x = Fingerprinter::new();
        x.write_str("ab");
        x.write_str("c");

        let mut y = Fingerprinter::new();
        y.write_str("a");
        y.write_str("bc");

        assert_ne!(x.finish(), y.finish());
    }

    #[test]
    fn hex_form_is_sixteen_lowercase_characters() {
        let fp = Fingerprinter::new();
        assert_eq!(fp.finish_hex(), "cbf29ce484222325");
    }
}
