//! Determinism: an identical seed reproduces a run bit-for-bit, and a different
//! seed changes it. Checked on both the single-VC and multi-VC kernels
//! (P0 exit gate; G1.4 for multiple VCs).

use tessera_cli::{run_point, RunParams};
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
fn identical_seed_is_reproducible() {
    let p = single_vc_params(7);
    let a = run_point(&p, 0.1);
    let b = run_point(&p, 0.1);
    assert_eq!(a.injected_packets, b.injected_packets);
    assert_eq!(a.ejected_packets, b.ejected_packets);
    assert_eq!(a.avg_packet_latency, b.avg_packet_latency);
    assert_eq!(a.accepted_flit_rate, b.accepted_flit_rate);

    // A different seed produces a different run.
    let c = run_point(&single_vc_params(8), 0.1);
    assert!(
        c.injected_packets != a.injected_packets || c.avg_packet_latency != a.avg_packet_latency,
        "different seed should change the run"
    );
}

#[test]
fn multi_vc_is_deterministic() {
    let a = run_point(&multi_vc_params(4, 11), 0.1);
    let b = run_point(&multi_vc_params(4, 11), 0.1);
    assert_eq!(a.injected_packets, b.injected_packets);
    assert_eq!(a.ejected_packets, b.ejected_packets);
    assert_eq!(a.avg_packet_latency, b.avg_packet_latency);
    assert_eq!(a.accepted_flit_rate, b.accepted_flit_rate);
}
