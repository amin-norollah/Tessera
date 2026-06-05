//! Deterministic per-node random number streams.
//!
//! One `splitmix64`-based stream per PE and per router, all derived from a
//! single master seed. Stream isolation makes runs reproducible and
//! order-independent under parallelism: unlike a single global `rand()`
//! (PAT-Noxim's fragile-determinism source). See `INITIAL_PRD` §20.10.

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// A single splittable, counter-based stream.
#[derive(Clone)]
pub struct NodeRng {
    state: u64,
}

impl NodeRng {
    /// Reconstruct a stream at an exact prior state (keyframe restore, P4-F1).
    pub fn from_state(state: u64) -> Self {
        Self { state }
    }

    /// This stream's raw counter state, for keyframing (P4-F1).
    #[inline]
    pub fn state(&self) -> u64 {
        self.state
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform `f64` in `[0, 1)`.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A uniform integer in `[0, n)`. Panics if `n == 0`.
    #[inline]
    pub fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % n as u64) as u32
    }
}

/// Deterministically mixes a label and index into a seed for a per-node stream.
fn mix(master: u64, label: u64, node: u32) -> u64 {
    let mut z =
        master ^ label.wrapping_mul(GOLDEN) ^ (node as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    z = (z ^ (z >> 32)).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    z ^ (z >> 32)
}

/// Factory for the per-node streams, seeded from one master value.
pub struct RngStreams {
    master: u64,
}

impl RngStreams {
    pub fn new(seed: u64) -> Self {
        Self { master: seed }
    }

    pub fn pe(&self, node: u32) -> NodeRng {
        NodeRng::from_state(mix(self.master, 0x5045, node))
    }

    pub fn router(&self, node: u32) -> NodeRng {
        NodeRng::from_state(mix(self.master, 0x5254, node))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_are_reproducible_and_isolated() {
        let a = RngStreams::new(42);
        let b = RngStreams::new(42);
        let mut ra = a.pe(3);
        let mut rb = b.pe(3);
        for _ in 0..1000 {
            assert_eq!(ra.next_u64(), rb.next_u64());
        }
        // Different nodes produce different streams.
        let mut r0 = a.pe(0);
        let mut r1 = a.pe(1);
        assert_ne!(r0.next_u64(), r1.next_u64());
    }
}
