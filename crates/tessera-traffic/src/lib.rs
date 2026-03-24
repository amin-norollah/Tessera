//! Traffic generators behind the [`TrafficGenerator`] contract. Phase 0 ships
//! uniform-random destinations with Bernoulli injection; the synthetic
//! permutation family and Netrace/SynFull import come later.

use tessera_core::{NodeRng, PacketDesc, RouterId, TrafficGenerator};

/// Uniform random traffic: each cycle a node injects one packet with
/// probability `rate` (packets/node/cycle), to a uniformly-chosen destination
/// other than itself.
pub struct Uniform {
    num_nodes: u32,
    rate: f64,
    packet_size: u16,
}

impl Uniform {
    pub fn new(num_nodes: u32, rate: f64, packet_size: u16) -> Self {
        assert!(num_nodes >= 2, "uniform traffic needs >= 2 nodes");
        assert!(
            (0.0..=1.0).contains(&rate),
            "injection rate must be in [0, 1]"
        );
        Self {
            num_nodes,
            rate,
            packet_size,
        }
    }
}

impl TrafficGenerator for Uniform {
    fn on_tick(&self, _cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        if rng.next_f64() >= self.rate {
            return;
        }
        // Uniform over the other n-1 nodes, without rejection sampling.
        let mut dst = rng.below(self.num_nodes - 1);
        if dst >= node.0 {
            dst += 1;
        }
        out.push(PacketDesc {
            dst: RouterId(dst),
            size: self.packet_size,
        });
    }
}
