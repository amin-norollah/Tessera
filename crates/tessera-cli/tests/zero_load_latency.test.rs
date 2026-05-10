//! Zero-load latency: with no contention, end-to-end packet latency must match
//! the closed-form pipeline + wormhole-serialization formula (P0 exit gate).

use tessera_core::{PacketDesc, RouterId, SimConfig, Simulation, TrafficGenerator};
use tessera_model::Mesh2D;
use tessera_routing::{DimensionOrderXy, FirstAvailable, RoundRobin};

/// Injects exactly one packet (node 0 → a fixed destination) at cycle 0.
struct OneShot {
    dst: RouterId,
    size: u16,
}

impl TrafficGenerator for OneShot {
    fn on_tick(
        &self,
        cycle: u64,
        node: RouterId,
        _rng: &mut tessera_core::NodeRng,
        out: &mut Vec<PacketDesc>,
    ) {
        if cycle == 0 && node.0 == 0 {
            out.push(PacketDesc {
                dst: self.dst,
                size: self.size,
            });
        }
    }
}

#[test]
fn zero_load_latency_matches_pipeline_formula() {
    let (rl, ll, size) = (2u16, 3u16, 4u16);
    let hops = 3u64; // node 0 (0,0) -> node 3 (3,0) on a 4-wide mesh
    let cfg = SimConfig {
        buffer_depth: 8,
        num_vcs: 1,
        router_latency: rl,
        link_latency: ll,
        credit_delay: 1,
        packet_size: size,
        seed: 1,
    };
    let mut sim = Simulation::new(
        cfg,
        Box::new(Mesh2D::new(4, 4)),
        Box::new(DimensionOrderXy),
        Box::new(FirstAvailable),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(OneShot {
            dst: RouterId(3),
            size,
        }),
    );
    sim.measure_start = 0;
    sim.measure_end = 1000;
    for _ in 0..80 {
        sim.tick();
    }

    // Uncontended: each of the (H+1) routers adds t_r, each of the H links adds
    // t_w, and the tail trails the head by (P-1) from wormhole serialization.
    let expected = (hops + 1) * rl as u64 + hops * ll as u64 + (size as u64 - 1);
    assert_eq!(sim.metrics.measured_packets, 1);
    assert_eq!(sim.metrics.latency_sum as u64, expected);
    assert_eq!(sim.in_flight(), 0, "single packet fully delivered");
}
