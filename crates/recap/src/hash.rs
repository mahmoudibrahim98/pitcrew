//! A fast hasher for maps keyed by ids. Ids are 128-bit ULIDs, and the default SipHash is several
//! times slower than needed for them. The state is seeded randomly per map, and the result is
//! mixed before use, so collisions cannot be planned from outside. Map order never reaches any
//! output: everything iterated from a map is sorted first.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher, RandomState};

pub(crate) type IdMap<K, V> = HashMap<K, V, IdState>;

/// Builds [`IdHasher`]s with a random seed.
#[derive(Clone, Debug)]
pub(crate) struct IdState {
    seed: u64,
}

impl Default for IdState {
    fn default() -> Self {
        Self {
            seed: RandomState::new().hash_one(0x5EED_u64),
        }
    }
}

impl BuildHasher for IdState {
    type Hasher = IdHasher;

    fn build_hasher(&self) -> IdHasher {
        IdHasher(self.seed)
    }
}

/// Multiply-rotate over 64-bit words, with a final avalanche.
pub(crate) struct IdHasher(u64);

impl IdHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517C_C1B7_2722_0A95);
    }
}

impl Hasher for IdHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word) ^ ((rest.len() as u64) << 56));
        }
    }

    fn write_u8(&mut self, n: u8) {
        self.add(u64::from(n));
    }

    fn write_u32(&mut self, n: u32) {
        self.add(u64::from(n));
    }

    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    fn write_u128(&mut self, n: u128) {
        self.add(n as u64);
        self.add((n >> 64) as u64);
    }

    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    fn write_isize(&mut self, n: isize) {
        self.add(n as u64);
    }

    fn finish(&self) -> u64 {
        // The finalizer of MurmurHash3.
        let mut h = self.0;
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 33;
        h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        h ^ (h >> 33)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_keys_hash_equal_and_ids_spread() {
        let state = IdState::default();
        let a = state.hash_one(42u128);
        assert_eq!(a, state.hash_one(42u128));
        let mut seen = std::collections::HashSet::new();
        for n in 0..10_000u128 {
            // ULIDs made in the same millisecond differ only in their low bits.
            seen.insert(state.hash_one((1u128 << 80) | n) >> 57);
        }
        assert_eq!(
            seen.len(),
            128,
            "the top 7 bits, which the map uses, must spread"
        );
        assert_ne!(
            state.hash_one(b"ab".as_slice()),
            state.hash_one(b"ba".as_slice())
        );
    }
}
