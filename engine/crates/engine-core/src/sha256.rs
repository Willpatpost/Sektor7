//! SHA-256 (FIPS 180-4) — the authenticated-summary primitive for
//! spine history compaction (PP-B2: the ratified v1.8 §5.6 prefix
//! summary over discarded execution records, and the unfinished-id
//! digest inside the compacted fingerprint domains).
//!
//! engine-core is zero-dependency by the ratified P0-3 discipline, so
//! the primitive lives here as a small, vector-pinned implementation
//! rather than a crate dependency. It is an internal mechanism: the
//! public fingerprint surface (FNV-1a, `fingerprint.rs`) is unchanged,
//! and nothing outside this crate reads these functions.

/// One-shot SHA-256 over `bytes`.
pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finish()
}

/// Streaming SHA-256. One instance hashes one message; `finish`
/// consumes the final block padding and returns the 32-byte digest.
pub(crate) struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    /// Total message length in bytes (the padding's big-endian bit
    /// count is derived from this).
    length: u64,
}

/// The round constants — the cube roots of the first 64 primes
/// (FIPS 180-4 §4.2.2).
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    /// The initial state — the square roots of the first 8 primes
    /// (FIPS 180-4 §5.3.1).
    pub(crate) fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    /// Absorbs message bytes. Buffers below one block; the length is
    /// tracked for the final padding.
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        let mut rest = bytes;
        if self.buffered > 0 {
            let want = 64 - self.buffered;
            let take = want.min(rest.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&rest[..take]);
            self.buffered += take;
            rest = &rest[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
        while rest.len() >= 64 {
            let mut block = [0; 64];
            block.copy_from_slice(&rest[..64]);
            self.compress(&block);
            rest = &rest[64..];
        }
        if !rest.is_empty() {
            self.buffer[..rest.len()].copy_from_slice(rest);
            self.buffered = rest.len();
        }
    }

    /// Finishes the message: pads (0x80, zeros, the big-endian bit
    /// length) and returns the digest. When the buffered message
    /// already reaches past byte 56, the padding spills into a second
    /// block (0x80 + zeros completing the first, then the length
    /// block).
    pub(crate) fn finish(mut self) -> [u8; 32] {
        let bit_length = self.length.wrapping_mul(8);
        self.buffer[self.buffered] = 0x80;
        for byte in &mut self.buffer[self.buffered + 1..] {
            *byte = 0;
        }
        if self.buffered < 56 {
            self.buffer[56..64].copy_from_slice(&bit_length.to_be_bytes());
            let block = self.buffer;
            self.compress(&block);
        } else {
            let block = self.buffer;
            self.compress(&block);
            let mut length_block = [0u8; 64];
            length_block[56..64].copy_from_slice(&bit_length.to_be_bytes());
            self.compress(&length_block);
        }
        let mut out = [0; 32];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *chunk = word.to_be_bytes();
        }
        out
    }

    /// The FIPS 180-4 §6.2.2 compression over one 64-byte block.
    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (chunk, word) in block.as_chunks::<4>().0.iter().zip(&mut w) {
            *word = u32::from_be_bytes(*chunk);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The published FIPS 180-4 / NIST example vectors.
    #[test]
    fn nist_vectors() {
        assert_eq!(
            hex(&digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&digest(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    /// The one-million-'a' vector (the multi-block streaming path).
    #[test]
    fn million_a_vector() {
        let mut hasher = Sha256::new();
        let chunk = [b'a'; 1000];
        for _ in 0..1000 {
            hasher.update(&chunk);
        }
        assert_eq!(
            hex(&hasher.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// The streaming API must equal the one-shot form for every split
    /// of the same message (the prefix-summary chain feeds it two
    /// pieces per record: the prior digest and the record bytes).
    #[test]
    fn streaming_splits_equal_one_shot() {
        let message: Vec<u8> = (0..200u8).collect();
        let one_shot = digest(&message);
        for split in [0, 1, 7, 63, 64, 65, 128, 199] {
            let mut hasher = Sha256::new();
            hasher.update(&message[..split]);
            hasher.update(&message[split..]);
            assert_eq!(hasher.finish(), one_shot, "split at {split}");
        }
        // Three-way splits too.
        let mut hasher = Sha256::new();
        hasher.update(&message[..10]);
        hasher.update(&message[10..74]);
        hasher.update(&message[74..]);
        assert_eq!(hasher.finish(), one_shot);
    }
}
