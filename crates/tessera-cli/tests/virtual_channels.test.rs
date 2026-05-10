//! Virtual channels: more VCs raise saturation throughput by relieving
//! head-of-line blocking (G1.2), and the per-VC state machine advances
//! Idle -> Routing -> Active -> Idle over a packet's lifetime (P1-F1).

use std::collections::HashSet;

use tessera_cli::{run_point, RunParams};
use tessera_core::{
    PacketDesc, PortId, RouterId, SimConfig, Simulation, TrafficGenerator, VcId, VcState,
};
use tessera_model::{Mesh2D, LOCAL};
use tessera_routing::{DimensionOrderXy, FirstAvailable, RoundRobin};

fn params(vcs: u8, seed: u64) -> RunParams {
    RunParams {
        width: 8,
        height: 8,
        cfg: SimConfig {
            buffer_depth: 8,
            num_vcs: vcs,
            router_latency: 1,
            link_latency: 1,
            credit_delay: 1,
            packet_size: 4,
            seed,
        },
        routing: "xy".to_string(),
        warmup: 1000,
        measure: 6000,
        drain_cap: 200_000,
    }
}

#[test]
fn more_vcs_raise_saturation_throughput() {
    // At a fixed offered load past the single-VC knee, accepted throughput must
    // rise with VC count (fewer head-of-line stalls).
    let rate = 0.12;
    let a1 = run_point(&params(1, 7), rate).accepted_flit_rate;
    let a2 = run_point(&params(2, 7), rate).accepted_flit_rate;
    let a4 = run_point(&params(4, 7), rate).accepted_flit_rate;
    assert!(a2 > a1 + 0.005, "2 VCs should beat 1: {a1} -> {a2}");
    assert!(a4 > a2 + 0.005, "4 VCs should beat 2: {a2} -> {a4}");
}

/// Injects one multi-flit packet (node 0 -> node 1) at cycle 0.
struct OneShot;

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
                dst: RouterId(1),
                size: 4,
            });
        }
    }
}

#[test]
fn vc_state_machine_advances() {
    let cfg = SimConfig {
        buffer_depth: 8,
        num_vcs: 2,
        router_latency: 2,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
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
        Box::new(OneShot),
    );

    let local = PortId(LOCAL);
    let vc0 = VcId(0);
    let node0 = RouterId(0);

    let mut seen = HashSet::new();
    seen.insert(format!("{:?}", sim.vc_state(node0, local, vc0))); // Idle before anything
    for _ in 0..40 {
        sim.tick();
        seen.insert(format!("{:?}", sim.vc_state(node0, local, vc0)));
    }

    assert!(seen.contains("Idle"), "starts and ends Idle");
    assert!(
        seen.contains("Routing"),
        "head waits for route (router latency)"
    );
    assert!(
        seen.contains("Active"),
        "body flits stream while port+VC held"
    );
    assert_eq!(
        sim.vc_state(node0, local, vc0),
        VcState::Idle,
        "fully drained"
    );
    assert_eq!(sim.in_flight(), 0);
}
