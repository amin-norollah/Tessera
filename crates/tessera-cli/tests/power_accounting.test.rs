//! End-to-end power accounting (P3-F1): the kernel accumulates per-router
//! activity counts as it runs, and the native power model converts them into
//! energy off the hot path, feeding the thermal grid. Verifies the counting is
//! self-consistent and deterministic, and that the whole PATh chain
//! (activity -> energy -> power -> temperature) produces physical results.

use tessera_cli::build_sim;
use tessera_core::{RouterId, SimConfig, Simulation};
use tessera_params::ParamDb;
use tessera_power::{EnergyBreakdown, NativePowerModel};
use tessera_thermal::{RcGrid, ThermalModel};

const PTM: &str = include_str!("../../../params/ptm_22nm.toml");

fn cfg() -> SimConfig {
    SimConfig {
        buffer_depth: 4,
        num_vcs: 2,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 7,
    }
}

fn run(cycles: u64) -> Simulation {
    let mut sim = build_sim(4, 4, cfg(), 0.2);
    sim.set_injecting(true);
    for _ in 0..cycles {
        sim.tick();
    }
    sim
}

#[test]
fn activity_counts_accumulate_and_are_self_consistent() {
    let sim = run(2000);
    let net = sim.network_activity();

    assert!(net.buf_write > 0);
    assert!(net.buf_read > 0);
    assert!(net.xbar > 0);
    assert!(net.sa_arb > 0);
    assert!(net.link_flits > 0);

    // Clock toggles once per router per cycle.
    assert_eq!(net.clock, 2000 * sim.num_nodes() as u64);

    // Each granted crossing reads a buffer, wins SA, and traverses the crossbar.
    assert_eq!(net.buf_read, net.xbar);
    assert_eq!(net.xbar, net.sa_arb);

    // Every crossbar traversal is either an ejection or a forward onto a link.
    assert_eq!(net.xbar, sim.metrics.ejected_flits + net.link_flits);

    // Forwarded flits are exactly the cumulative inter-router hops.
    assert_eq!(net.link_flits, sim.metrics.total_hops);
}

#[test]
fn counts_and_energy_are_deterministic() {
    let a = run(1500);
    let b = run(1500);
    assert_eq!(
        a.network_activity(),
        b.network_activity(),
        "activity counts must be reproducible for a fixed seed"
    );
    // Per-router counts too, not just the network sum.
    assert_eq!(a.activity(), b.activity());
}

#[test]
fn energy_breakdown_is_positive_across_subsystems() {
    let p = ParamDb::from_toml(PTM).unwrap();
    let model = NativePowerModel::default();
    let sim = run(2000);

    let mut total = EnergyBreakdown::default();
    for (r, counts) in sim.activity().iter().enumerate() {
        total.add(&model.energy_from_counts(counts, RouterId(r as u32), &p));
    }

    assert!(total.total() > 0.0);
    assert!(total.clock > 0.0, "clock always dissipates");
    assert!(total.buffers > 0.0);
    assert!(total.crossbar > 0.0);
    assert!(total.arbitration > 0.0);
    assert!(total.links > 0.0);

    let power = model.average_power(total.total(), sim.cycle, &p);
    assert!(power > 0.0 && power.is_finite(), "average power {power}");
}

#[test]
fn per_router_power_drives_the_thermal_grid() {
    let p = ParamDb::from_toml(PTM).unwrap();
    let model = NativePowerModel::default();
    let sim = run(3000);
    let cycles = sim.cycle;

    // Convert each router's accumulated energy into an average power draw.
    let power: Vec<f64> = sim
        .activity()
        .iter()
        .enumerate()
        .map(|(r, counts)| {
            let e = model
                .energy_from_counts(counts, RouterId(r as u32), &p)
                .total();
            model.average_power(e, cycles, &p)
        })
        .collect();
    assert_eq!(power.len(), sim.num_nodes() as usize);
    assert!(power.iter().all(|&w| w > 0.0 && w.is_finite()));

    // Drive the RC thermal network with that per-tile power: the chain must
    // yield finite temperatures at or above ambient.
    let amb = p.thermal.t_amb_c.value as f32;
    let mut grid = RcGrid::from_params(4, 4, &p, true);
    let mut temps = grid.temperatures().to_vec();
    for _ in 0..200 {
        temps = grid.step(&power, 1e-4).to_vec();
    }
    assert!(temps.iter().all(|&t| t >= amb && t.is_finite()));

    // The native defaults put router power in the microwatt range, so the rise
    // on top of a 45 C ambient is below f32 resolution — assert the thermal
    // path is live with a plainly non-trivial (mW-scale) uniform load instead.
    let mut hot = RcGrid::from_params(4, 4, &p, true);
    let load = vec![1e-3_f64; grid.num_tiles()];
    let mut warmed = hot.temperatures().to_vec();
    for _ in 0..500 {
        warmed = hot.step(&load, 1e-4).to_vec();
    }
    assert!(
        warmed.iter().any(|&t| t > amb),
        "a mW-scale load should measurably heat the die: {warmed:?}"
    );
}
