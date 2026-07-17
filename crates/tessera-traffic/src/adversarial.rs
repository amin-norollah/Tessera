//! Non-uniform synthetic patterns used to stress-test adaptive/turn-model
//! routing against plain dimension-order routing (INITIAL_PRD §20.8).

use tessera_core::{NodeRng, PacketDesc, RouterId, TrafficGenerator};

/// Transpose traffic on a square `side`x`side` mesh: node `(x, y)` sends to
/// `(y, x)`. A classic adversarial pattern: it concentrates load on
/// diagonal-crossing links in a way uniform traffic never does, so it's
/// useful for telling adaptive/turn-model routing apart from plain DOR.
pub struct Transpose {
    side: u32,
    rate: f64,
    packet_size: u16,
}

impl Transpose {
    pub fn new(side: u32, rate: f64, packet_size: u16) -> Self {
        assert!(side >= 2, "transpose traffic needs a >= 2x2 mesh");
        assert!(
            (0.0..=1.0).contains(&rate),
            "injection rate must be in [0, 1]"
        );
        Self {
            side,
            rate,
            packet_size,
        }
    }
}

impl TrafficGenerator for Transpose {
    fn on_tick(&self, _cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        if rng.next_f64() >= self.rate {
            return;
        }
        let x = node.0 % self.side;
        let y = node.0 / self.side;
        let dst = x * self.side + y;
        if dst == node.0 {
            return; // on the diagonal: no distinct transpose partner
        }
        out.push(PacketDesc {
            dst: RouterId(dst),
            size: self.packet_size,
            tag: 0,
        });
    }
}

/// Hotspot traffic: `hotspot_share` of each node's injected packets target a
/// single fixed hotspot node; the rest are uniform over the other nodes.
/// Concentrates load on the links leading to one node, unlike uniform.
pub struct Hotspot {
    num_nodes: u32,
    rate: f64,
    packet_size: u16,
    hotspot: RouterId,
    hotspot_share: f64,
}

impl Hotspot {
    pub fn new(
        num_nodes: u32,
        rate: f64,
        packet_size: u16,
        hotspot: RouterId,
        hotspot_share: f64,
    ) -> Self {
        assert!(num_nodes >= 2, "hotspot traffic needs >= 2 nodes");
        assert!(
            (0.0..=1.0).contains(&rate),
            "injection rate must be in [0, 1]"
        );
        assert!(
            (0.0..=1.0).contains(&hotspot_share),
            "hotspot_share must be in [0, 1]"
        );
        Self {
            num_nodes,
            rate,
            packet_size,
            hotspot,
            hotspot_share,
        }
    }
}

impl TrafficGenerator for Hotspot {
    fn on_tick(&self, _cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        if rng.next_f64() >= self.rate {
            return;
        }
        let dst = if node.0 != self.hotspot.0 && rng.next_f64() < self.hotspot_share {
            self.hotspot
        } else {
            let mut d = rng.below(self.num_nodes - 1);
            if d >= node.0 {
                d += 1;
            }
            RouterId(d)
        };
        out.push(PacketDesc {
            dst,
            size: self.packet_size,
            tag: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::RngStreams;

    #[test]
    fn transpose_swaps_coordinates_on_a_square_mesh() {
        let gen = Transpose::new(4, 1.0, 4);
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();
        // Node (1,2) -> id 2*4+1=9; transpose partner (2,1) -> id 1*4+2=6.
        gen.on_tick(0, RouterId(9), &mut rng, &mut out);
        assert_eq!(out[0].dst, RouterId(6));
    }

    #[test]
    fn transpose_never_targets_the_diagonal_itself() {
        let gen = Transpose::new(4, 1.0, 4);
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();
        gen.on_tick(0, RouterId(0), &mut rng, &mut out); // (0,0) is on the diagonal
        assert!(out.is_empty());
    }

    #[test]
    fn hotspot_sends_only_to_the_hotspot_when_share_is_one() {
        let gen = Hotspot::new(16, 1.0, 4, RouterId(3), 1.0);
        let mut rng = RngStreams::new(1).pe(5);
        let mut out = Vec::new();
        for cycle in 0..20 {
            out.clear();
            gen.on_tick(cycle, RouterId(5), &mut rng, &mut out);
            assert_eq!(out[0].dst, RouterId(3));
        }
    }

    #[test]
    fn hotspot_node_itself_never_targets_itself() {
        let gen = Hotspot::new(16, 1.0, 4, RouterId(3), 1.0);
        let mut rng = RngStreams::new(1).pe(3);
        let mut out = Vec::new();
        for cycle in 0..20 {
            out.clear();
            gen.on_tick(cycle, RouterId(3), &mut rng, &mut out);
            assert_ne!(out[0].dst, RouterId(3));
        }
    }
}
