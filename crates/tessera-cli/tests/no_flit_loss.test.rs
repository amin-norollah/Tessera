//! No-flit-loss / full drain: after injection stops, the network drains
//! completely and every injected packet was ejected: no flit is lost or
//! stranded. Checked on both the single-VC and multi-VC kernels
//! (P0 exit gate; G1.5 for multiple VCs).

use tessera_cli::{build_sim, RunParams};
use tessera_core::SimConfig;

fn single_vc_params(seed: u64) -> RunParams {
    RunParams {
        width: 6,
        height: 6,
        cfg: SimConfig {
            buffer_depth: 4,
            num_vcs: 1,
            router_latency: 1,
            link_latency: 1,
            credit_delay: 1,
            packet_size: 4,
            seed,
        },
        routing: "xy".to_string(),
        warmup: 500,
        measure: 3000,
        drain_cap: 100_000,
    }
}

fn multi_vc_params(vcs: u8, seed: u64) -> RunParams {
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
fn low_load_drains_with_no_flit_loss() {
    let p = single_vc_params(3);
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

#[test]
fn multi_vc_drains_with_no_flit_loss() {
    let p = multi_vc_params(2, 5);
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
