//! Phase 0 exit-gate checks: zero-load latency (pipelining + serialization),
//! determinism, and no-flit-loss / full drain.

use tessera_cli::{build_sim, run_point, RunParams};
use tessera_core::{PacketDesc, RouterId, SimConfig, Simulation, TrafficGenerator};
use tessera_model::Mesh2D;
use tessera_routing::{DimensionOrderXy, FirstAvailable};

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

fn params(seed: u64) -> RunParams {
    RunParams {
        width: 6,
        height: 6,
        cfg: SimConfig {
            buffer_depth: 4,
            router_latency: 1,
            link_latency: 1,
            credit_delay: 1,
            packet_size: 4,
            seed,
        },
        warmup: 500,
        measure: 3000,
        drain_cap: 100_000,
    }
}

#[test]
fn identical_seed_is_reproducible() {
    let p = params(7);
    let a = run_point(&p, 0.1);
    let b = run_point(&p, 0.1);
    assert_eq!(a.injected_packets, b.injected_packets);
    assert_eq!(a.ejected_packets, b.ejected_packets);
    assert_eq!(a.avg_packet_latency, b.avg_packet_latency);
    assert_eq!(a.accepted_flit_rate, b.accepted_flit_rate);

    // A different seed produces a different run.
    let c = run_point(&params(8), 0.1);
    assert!(
        c.injected_packets != a.injected_packets || c.avg_packet_latency != a.avg_packet_latency,
        "different seed should change the run"
    );
}

#[test]
fn low_load_drains_with_no_flit_loss() {
    let p = params(3);
    let mut sim = build_sim(p.width, p.height, p.cfg, 0.05);
    sim.measure_start = p.warmup;
    sim.measure_end = p.warmup + p.measure;

    sim.set_injecting(true);
    for _ in 0..(p.warmup + p.measure) {
        sim.tick();
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < p.drain_cap {
        sim.tick();
        guard += 1;
    }

    assert_eq!(sim.in_flight(), 0, "network fully drained");
    assert_eq!(sim.metrics.injected_packets, sim.metrics.ejected_packets);
    assert!(sim.metrics.injected_packets > 0);
}
