//! The `Allocator` trait must serve VA and both SA phases interchangeably
//! (P1-F6): swapping `RoundRobin` for `Matrix`/`ISlip` should still deliver
//! every packet and drain cleanly on a multi-VC mesh, the same property
//! `phase1_routing.rs` checks for the routing family.

use tessera_cli::build_sim_with_allocators;
use tessera_core::{Allocator, SimConfig};
use tessera_routing::{DimensionOrderXy, ISlip, Matrix, RoundRobin};

fn drains_cleanly(va: Box<dyn Allocator>, sa1: Box<dyn Allocator>, sa2: Box<dyn Allocator>) {
    let cfg = SimConfig {
        buffer_depth: 4,
        num_vcs: 4,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 13,
    };
    let mut sim =
        build_sim_with_allocators(8, 8, cfg, 0.15, Box::new(DimensionOrderXy), va, sa1, sa2);
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

#[test]
fn round_robin_drains_cleanly() {
    drains_cleanly(
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
    );
}

#[test]
fn matrix_drains_cleanly() {
    drains_cleanly(
        Box::new(Matrix::new()),
        Box::new(Matrix::new()),
        Box::new(Matrix::new()),
    );
}

#[test]
fn islip_drains_cleanly() {
    drains_cleanly(
        Box::new(ISlip::new(2)),
        Box::new(ISlip::new(2)),
        Box::new(ISlip::new(2)),
    );
}
