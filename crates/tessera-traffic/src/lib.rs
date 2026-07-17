//! Traffic generators behind the [`TrafficGenerator`] contract: Phase 0/1
//! synthetic patterns (`Uniform`, [`Hotspot`], [`Transpose`], [`Bimodal`])
//! plus Phase 3.5 traffic realism: [`netrace::Netrace`] (dependency-driven
//! trace replay) and [`synfull::SynFull`] (Markov-model synthetic traffic),
//! composable with [`closed_loop::ClosedLoop`] (outstanding-message limits)
//! and [`reorder::ReorderBuffer`] (destination reassembly).

use tessera_core::{NodeRng, PacketDesc, RouterId, TrafficGenerator};

mod adversarial;
mod bimodal;
pub mod closed_loop;
pub mod netrace;
pub mod reorder;
pub mod synfull;

pub use adversarial::{Hotspot, Transpose};
pub use bimodal::Bimodal;
pub use closed_loop::ClosedLoop;
pub use reorder::ReorderBuffer;

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
            tag: 0,
        });
    }
}
