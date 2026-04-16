//! Phase 1 checks for the multi-VC kernel: virtual channels raise throughput
//! (G1.2), determinism and no-flit-loss survive multiple VCs (G1.4/G1.5), and
//! the VC state machine advances Idle -> Routing -> Active -> Idle (P1-F1).

use std::collections::HashSet;

use tessera_cli::{build_sim, run_point, RunParams};
use tessera_core::{
    PacketDesc, PortId, RouterId, SimConfig, Simulation, TrafficGenerator, VcId, VcState,
};
use tessera_model::{Mesh2D, LOCAL};
use tessera_routing::{DimensionOrderXy, FirstAvailable};

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

#[test]
fn multi_vc_is_deterministic() {
    let a = run_point(&params(4, 11), 0.1);
    let b = run_point(&params(4, 11), 0.1);
    assert_eq!(a.injected_packets, b.injected_packets);
    assert_eq!(a.ejected_packets, b.ejected_packets);
    assert_eq!(a.avg_packet_latency, b.avg_packet_latency);
    assert_eq!(a.accepted_flit_rate, b.accepted_flit_rate);
}

#[test]
fn multi_vc_drains_with_no_flit_loss() {
    let p = params(2, 5);
    let mut sim = build_sim(p.width, p.height, p.cfg, 0.05);
    sim.set_injecting(true);
    for _ in 0..3000 {
        sim.tick();
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < p.drain_cap {
        sim.tick();
        guard += 1;
    }
    assert_eq!(sim.in_flight(), 0);
    assert_eq!(sim.metrics.injected_packets, sim.metrics.ejected_packets);
    assert!(sim.metrics.injected_packets > 0);
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
