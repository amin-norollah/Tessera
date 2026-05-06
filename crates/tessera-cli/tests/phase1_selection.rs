//! The `SelectionPolicy` family (P1-F11) must all drive a real multi-VC run
//! cleanly: every packet delivered, network fully drained. Mirrors
//! `phase1_allocators.rs`'s treatment of the `Allocator` family.
//!
//! Exercised against both a partial turn model (west-first, which has a
//! *mandatory* direction whenever one is pending — `turn_route` must not
//! offer its deferrable direction as a free choice, or a congestion-seeking
//! policy like buffer-level will happily defer it forever and deadlock the
//! network) and odd-even (whose two candidates, when both legal, are a
//! genuine tie — free choice between them is safe).

use tessera_core::{RoutingAlgorithm, SelectionPolicy, SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_routing::{
    BufferLevel, FirstAvailable, NegativeFirst, NorthLast, OddEven, Random, RoundRobin,
    RoundRobinSelection, WestFirst,
};
use tessera_traffic::Uniform;

fn drains_cleanly(routing: Box<dyn RoutingAlgorithm>, selection: Box<dyn SelectionPolicy>) {
    let cfg = SimConfig {
        buffer_depth: 4,
        num_vcs: 2,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 17,
    };
    let mut sim = Simulation::new(
        cfg,
        Box::new(Mesh2D::new(8, 8)),
        routing,
        selection,
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(Uniform::new(64, 0.12, 4)),
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
    assert_eq!(sim.in_flight(), 0, "did not drain (possible deadlock)");
    assert_eq!(
        sim.metrics.injected_packets, sim.metrics.ejected_packets,
        "lost packets"
    );
    assert!(sim.metrics.ejected_packets > 0, "delivered nothing");
}

macro_rules! selection_tests {
    ($mod_name:ident, $routing:expr) => {
        mod $mod_name {
            use super::*;

            #[test]
            fn first_available_drains_cleanly() {
                drains_cleanly($routing, Box::new(FirstAvailable));
            }

            #[test]
            fn buffer_level_drains_cleanly() {
                drains_cleanly($routing, Box::new(BufferLevel));
            }

            #[test]
            fn round_robin_selection_drains_cleanly() {
                drains_cleanly($routing, Box::new(RoundRobinSelection::new()));
            }

            #[test]
            fn random_selection_drains_cleanly() {
                drains_cleanly($routing, Box::new(Random));
            }
        }
    };
}

selection_tests!(with_west_first, Box::new(WestFirst));
selection_tests!(with_north_last, Box::new(NorthLast));
selection_tests!(with_negative_first, Box::new(NegativeFirst));
selection_tests!(with_odd_even, Box::new(OddEven));
