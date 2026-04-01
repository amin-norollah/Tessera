//! Run-driving helpers shared by the `tessera` binary and the integration
//! tests: assemble a [`Simulation`] from Phase 0 components and execute the
//! standard warm-up / measurement / drain harness for one offered load.

use tessera_core::{SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_routing::{DimensionOrderXy, FirstAvailable};
use tessera_stats::RunPoint;
use tessera_traffic::Uniform;

/// Everything constant across an injection-rate sweep.
#[derive(Clone, Copy, Debug)]
pub struct RunParams {
    pub width: u16,
    pub height: u16,
    pub cfg: SimConfig,
    pub warmup: u64,
    pub measure: u64,
    pub drain_cap: u64,
}

pub fn build_sim(width: u16, height: u16, cfg: SimConfig, rate: f64) -> Simulation {
    let nodes = width as u32 * height as u32;
    Simulation::new(
        cfg,
        Box::new(Mesh2D::new(width, height)),
        Box::new(DimensionOrderXy),
        Box::new(FirstAvailable),
        Box::new(Uniform::new(nodes, rate, cfg.packet_size)),
    )
}

/// Run one offered-load point through warm-up, measurement, and drain phases.
pub fn run_point(p: &RunParams, rate: f64) -> RunPoint {
    let mut sim = build_sim(p.width, p.height, p.cfg, rate);
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

    let nodes = p.width as u64 * p.height as u64;
    RunPoint::from_metrics(&sim.metrics, nodes, p.measure, p.cfg.packet_size, rate)
}
