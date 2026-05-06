//! Bimodal packet sizes (P1-F17) must drive a real run cleanly, mixing
//! control- and data-sized packets end to end.

use tessera_core::SimConfig;
use tessera_model::Mesh2D;
use tessera_routing::{DimensionOrderXy, FirstAvailable, RoundRobin};
use tessera_traffic::{Bimodal, Uniform};

#[test]
fn bimodal_traffic_drains_cleanly() {
    let cfg = SimConfig {
        buffer_depth: 8,
        num_vcs: 1,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4, // overridden per-packet by Bimodal
        seed: 9,
    };
    let traffic = Bimodal::new(Uniform::new(64, 0.08, 4), 1, 8, 0.5);
    let mut sim = tessera_core::Simulation::new(
        cfg,
        Box::new(Mesh2D::new(8, 8)),
        Box::new(DimensionOrderXy),
        Box::new(FirstAvailable),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(traffic),
    );
    sim.set_injecting(true);
    for _ in 0..4000 {
        sim.tick();
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < 200_000 {
        sim.tick();
        guard += 1;
    }

    assert_eq!(sim.in_flight(), 0, "did not drain");
    assert_eq!(
        sim.metrics.injected_packets, sim.metrics.ejected_packets,
        "lost packets"
    );
    assert!(sim.metrics.injected_packets > 0);
    // Average flits/packet should land strictly between the two size
    // classes, confirming both were actually used (not just one).
    let avg_size = sim.metrics.injected_flits as f64 / sim.metrics.injected_packets as f64;
    assert!(
        avg_size > 1.0 && avg_size < 8.0,
        "avg packet size {avg_size} suggests only one size class was drawn"
    );
}
