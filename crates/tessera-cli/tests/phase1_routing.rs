//! Deadlock / stranding property check for the routing family: on a single-VC
//! mesh, each router must deliver every injected packet and let the network
//! fully drain. A deadlocked or stranding routing function would leave flits
//! in flight past the drain cap and fail here (P1-F9, G1.4).

use tessera_cli::{build_sim_with, RoutingKind};
use tessera_core::SimConfig;

fn drains_cleanly(kind: RoutingKind) {
    let cfg = SimConfig {
        buffer_depth: 4,
        num_vcs: 1,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 9,
    };
    let mut sim = build_sim_with(8, 8, cfg, 0.08, kind.build());
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
    assert_eq!(
        sim.in_flight(),
        0,
        "{kind:?} did not drain (possible deadlock)"
    );
    assert_eq!(
        sim.metrics.injected_packets, sim.metrics.ejected_packets,
        "{kind:?} lost packets"
    );
    assert!(
        sim.metrics.ejected_packets > 0,
        "{kind:?} delivered nothing"
    );
}

#[test]
fn every_router_delivers_and_drains() {
    for kind in [
        RoutingKind::Xy,
        RoutingKind::Xyz,
        RoutingKind::WestFirst,
        RoutingKind::NorthLast,
        RoutingKind::NegativeFirst,
    ] {
        drains_cleanly(kind);
    }
}
