//! Fast pseudo-random generator for test patterns and test order.
//!
//! This is Marsaglia's xorshift128. It is not cryptographically secure, and it
//! does not need to be. FlashValidator seeds it from operating-system entropy
//! for every run, so a drive controller cannot predict the patterns.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// Xorshift128 pseudo-random number generator.
#[derive(Debug, Clone)]
pub struct Xorshift128 {
    x: u32,
    y: u32,
    z: u32,
    w: u32,
}

impl Xorshift128 {
    /// Creates a generator from a 64-bit seed, expanded with SplitMix64.
    pub fn from_seed(seed: u64) -> Self {
        let mut s = seed;
        let a = splitmix64(&mut s);
        let b = splitmix64(&mut s);
        let mut rng = Self { x: a as u32, y: (a >> 32) as u32, z: b as u32, w: (b >> 32) as u32 };
        if rng.x | rng.y | rng.z | rng.w == 0 {
            rng.w = 0x9E37_79B9; // the all-zero state is a fixed point
        }
        rng
    }

    /// Creates a generator seeded from operating-system entropy.
    pub fn from_entropy() -> Self {
        Self::from_seed(entropy_seed())
    }

    /// Returns the next 32-bit value.
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let t = self.x ^ (self.x << 11);
        self.x = self.y;
        self.y = self.z;
        self.z = self.w;
        self.w = self.w ^ (self.w >> 19) ^ t ^ (t >> 8);
        self.w
    }

    /// Returns the next 64-bit value.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        (u64::from(self.next_u32()) << 32) | u64::from(self.next_u32())
    }

    /// Returns a value in `0..bound` (multiply-shift reduction: `rand * n >> 32`).
    ///
    /// # Panics
    ///
    /// Panics if `bound` is zero or larger than `u32::MAX`.
    pub fn below(&mut self, bound: usize) -> usize {
        assert!(bound > 0 && bound <= u32::MAX as usize);
        ((u64::from(self.next_u32()) * bound as u64) >> 32) as usize
    }

    /// Fills `buf` with pseudo-random bytes.
    pub fn fill(&mut self, buf: &mut [u8]) {
        let mut chunks = buf.chunks_exact_mut(4);
        for c in &mut chunks {
            c.copy_from_slice(&self.next_u32().to_le_bytes());
        }
        let rest = chunks.into_remainder();
        if !rest.is_empty() {
            let v = self.next_u32().to_le_bytes();
            rest.copy_from_slice(&v[..rest.len()]);
        }
    }
}

/// Returns a 64-bit seed from operating-system entropy.
///
/// `RandomState` gets its keys from the OS random source, so no extra
/// dependency is needed. Time and process id are mixed in as well.
pub fn entropy_seed() -> u64 {
    let mut h = RandomState::new().build_hasher();
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    h.write_u32(std::process::id());
    h.finish()
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_xorshift128() {
        // Marsaglia's reference initial state.
        let mut r = Xorshift128 { x: 123_456_789, y: 362_436_069, z: 521_288_629, w: 88_675_123 };
        assert_eq!(r.next_u32(), 3_701_687_786);
        assert_eq!(r.next_u32(), 458_299_110);
    }

    #[test]
    fn deterministic_for_seed() {
        let mut a = Xorshift128::from_seed(42);
        let mut b = Xorshift128::from_seed(42);
        let (mut x, mut y) = ([0u8; 37], [0u8; 37]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_eq!(x, y);
        assert_ne!(x, [0u8; 37]);
    }

    #[test]
    fn below_stays_in_range() {
        let mut r = Xorshift128::from_seed(7);
        for n in 1..200 {
            assert!(r.below(n) < n);
        }
    }
}
