//! Deterministic RNG using FNV-1a hash and SplitMix64
//!
//! Uses a stable hash (not std's randomized hasher) for cross-run reproducibility.

/// FNV-1a hash of a string to a u64 seed.
/// This is deterministic across runs (unlike std's DefaultHasher).
pub fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Derive a child seed from a parent seed and fork index.
pub fn derive_seed(parent_seed: &str, fork_index: u64) -> String {
    format!("{}::fork:{}", parent_seed, fork_index)
}

/// A simple deterministic PRNG using SplitMix64.
/// Produces high-quality pseudo-random numbers suitable for timing engine use.
#[derive(Clone, Debug)]
pub struct DetRng {
    state: u64,
}

impl DetRng {
    /// Create a new RNG from a string seed.
    pub fn new(seed: &str) -> Self {
        Self {
            state: fnv1a64(seed),
        }
    }

    /// Create a new RNG from a u64 seed.
    pub fn from_u64(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Generate the next u64 value (SplitMix64 algorithm).
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Generate a random f64 in [0, 1).
    pub fn random(&mut self) -> f64 {
        // Use upper 53 bits for double precision
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Fork this RNG to create a child with a derived seed.
    pub fn fork(&self, seed: &str, fork_index: u64) -> Self {
        let child_seed = derive_seed(seed, fork_index);
        Self::new(&child_seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a64_deterministic() {
        let h1 = fnv1a64("test_seed");
        let h2 = fnv1a64("test_seed");
        assert_eq!(h1, h2);

        let h3 = fnv1a64("different_seed");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_rng_deterministic() {
        let mut rng1 = DetRng::new("test");
        let mut rng2 = DetRng::new("test");

        for _ in 0..100 {
            assert_eq!(rng1.next_u64(), rng2.next_u64());
        }
    }

    #[test]
    fn test_rng_random_range() {
        let mut rng = DetRng::new("test");

        for _ in 0..1000 {
            let r = rng.random();
            assert!(r >= 0.0 && r < 1.0);
        }
    }

    #[test]
    fn test_fork_deterministic() {
        let rng = DetRng::new("parent");

        let mut fork1a = rng.fork("parent", 0);
        let mut fork1b = rng.fork("parent", 0);

        for _ in 0..100 {
            assert_eq!(fork1a.next_u64(), fork1b.next_u64());
        }
    }

    #[test]
    fn test_fork_different_indices() {
        let rng = DetRng::new("parent");

        let mut fork0 = rng.fork("parent", 0);
        let mut fork1 = rng.fork("parent", 1);

        // Different forks should produce different sequences
        let v0: Vec<_> = (0..10).map(|_| fork0.next_u64()).collect();
        let v1: Vec<_> = (0..10).map(|_| fork1.next_u64()).collect();

        assert_ne!(v0, v1);
    }
}
