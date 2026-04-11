//! Run-driving helpers shared by the `tessera` binary and the integration
//! tests: assemble a [`Simulation`] and execute the standard warm-up /
//! measurement / drain harness for one offered load.

use tessera_core::{RoutingAlgorithm, SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_routing::{
    DimensionOrderXy, DimensionOrderXyz, FirstAvailable, NegativeFirst, NorthLast, WestFirst,
};
use tessera_stats::RunPoint;
use tessera_traffic::Uniform;

/// Which routing algorithm to run. A small in-tree selector; the string-keyed
/// registry that replaces it lands with the plugin API in Phase 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutingKind {
    Xy,
    Xyz,
    WestFirst,
    NorthLast,
    NegativeFirst,
}

impl RoutingKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "xy" => RoutingKind::Xy,
            "xyz" => RoutingKind::Xyz,
            "west-first" | "westfirst" => RoutingKind::WestFirst,
            "north-last" | "northlast" => RoutingKind::NorthLast,
            "negative-first" | "negativefirst" => RoutingKind::NegativeFirst,
            _ => return None,
        })
    }

    pub fn build(self) -> Box<dyn RoutingAlgorithm> {
        match self {
            RoutingKind::Xy => Box::new(DimensionOrderXy),
            RoutingKind::Xyz => Box::new(DimensionOrderXyz),
            RoutingKind::WestFirst => Box::new(WestFirst),
            RoutingKind::NorthLast => Box::new(NorthLast),
            RoutingKind::NegativeFirst => Box::new(NegativeFirst),
        }
    }
}

/// Everything constant across an injection-rate sweep.
#[derive(Clone, Copy, Debug)]
pub struct RunParams {
    pub width: u16,
    pub height: u16,
    pub cfg: SimConfig,
    pub routing: RoutingKind,
    pub warmup: u64,
    pub measure: u64,
    pub drain_cap: u64,
}

pub fn build_sim_with(
    width: u16,
    height: u16,
    cfg: SimConfig,
    rate: f64,
    routing: Box<dyn RoutingAlgorithm>,
) -> Simulation {
    let nodes = width as u32 * height as u32;
    Simulation::new(
        cfg,
        Box::new(Mesh2D::new(width, height)),
        routing,
        Box::new(FirstAvailable),
        Box::new(Uniform::new(nodes, rate, cfg.packet_size)),
    )
}

pub fn build_sim(width: u16, height: u16, cfg: SimConfig, rate: f64) -> Simulation {
    build_sim_with(width, height, cfg, rate, RoutingKind::Xy.build())
}

/// Run one offered-load point through warm-up, measurement, and drain phases.
pub fn run_point(p: &RunParams, rate: f64) -> RunPoint {
    let mut sim = build_sim_with(p.width, p.height, p.cfg, rate, p.routing.build());
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
