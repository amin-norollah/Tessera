//! Property tests (G1.4): credit conservation
//! ([`tessera_core::Simulation::debug_invariants`]) checked every cycle
//! across real runs for several routing algorithms.
//!
//! The wormhole invariant (a packet's flits never interleave with another
//! packet's on a shared VC) is enforced structurally by construction — a VC
//! is granted to exactly one holder at a time (`Simulation`'s `hold` array;
//! `stage_vc_alloc` only ever assigns a `FREE` slot) — and empirically by
//! every no-flit-loss assertion across this test suite, which a genuine
//! interleaving bug would corrupt. A dedicated per-packet delivery-order
//! trace would need new introspection API this backfill doesn't add; noted
//! as a natural follow-up alongside the diagnostics work in Phase 3.

use tessera_core::{SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_routing::{DimensionOrderXy, FirstAvailable, OddEven, RoundRobin, WestFirst};
use tessera_traffic::Uniform;

fn build(routing: Box<dyn tessera_core::RoutingAlgorithm>, num_vcs: u8, seed: u64) -> Simulation {
    let cfg = SimConfig {
        buffer_depth: 4,
        num_vcs,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed,
    };
    Simulation::new(
        cfg,
        Box::new(Mesh2D::new(6, 6)),
        routing,
        Box::new(FirstAvailable),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(Uniform::new(36, 0.15, 4)),
    )
}

fn credit_conservation_holds_every_cycle(routing: Box<dyn tessera_core::RoutingAlgorithm>) {
    let mut sim = build(routing, 2, 23);
    sim.set_injecting(true);
    for cycle in 0..3000 {
        sim.tick();
        sim.debug_invariants()
            .unwrap_or_else(|e| panic!("cycle {cycle}: {e}"));
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < 50_000 {
        sim.tick();
        sim.debug_invariants()
            .unwrap_or_else(|e| panic!("drain cycle {guard}: {e}"));
        guard += 1;
    }
}

#[test]
fn credit_conservation_holds_for_xy() {
    credit_conservation_holds_every_cycle(Box::new(DimensionOrderXy));
}

#[test]
fn credit_conservation_holds_for_west_first() {
    credit_conservation_holds_every_cycle(Box::new(WestFirst));
}

#[test]
fn credit_conservation_holds_for_odd_even() {
    credit_conservation_holds_every_cycle(Box::new(OddEven));
}
