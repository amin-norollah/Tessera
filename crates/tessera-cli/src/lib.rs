//! Run-driving helpers shared by the `tessera` binary and the integration
//! tests: assemble a [`Simulation`] and execute the standard warm-up /
//! measurement / drain harness for one offered load. Routing components are
//! resolved by string key through the plugin [`Registry`](tessera_plugin::Registry).
//! [`build`] assembles a run from a full [`tessera_config::SimConfigFile`]
//! instead (every component choice, including file-backed traffic).

pub mod build;

pub use build::{
    build_from_config, build_from_path, load_config, run_configured, BuildError, ConfiguredRun,
};

use tessera_core::{Allocator, RoutingAlgorithm, SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_plugin::{builtin_registry, PluginCtx};
use tessera_routing::{FirstAvailable, RoundRobin};
use tessera_stats::RunPoint;
use tessera_traffic::Uniform;

/// Everything constant across an injection-rate sweep.
#[derive(Clone, Debug)]
pub struct RunParams {
    pub width: u16,
    pub height: u16,
    pub cfg: SimConfig,
    /// Routing algorithm key, resolved through the plugin registry.
    pub routing: String,
    pub warmup: u64,
    pub measure: u64,
    pub drain_cap: u64,
}

fn plugin_ctx(width: u16, height: u16, cfg: SimConfig, rate: f64) -> PluginCtx {
    PluginCtx {
        num_nodes: width as u32 * height as u32,
        num_vcs: cfg.num_vcs,
        packet_size: cfg.packet_size,
        injection_rate: rate,
    }
}

pub fn build_sim_with(
    width: u16,
    height: u16,
    cfg: SimConfig,
    rate: f64,
    routing: Box<dyn RoutingAlgorithm>,
) -> Simulation {
    build_sim_with_allocators(
        width,
        height,
        cfg,
        rate,
        routing,
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
    )
}

/// As [`build_sim_with`], but with explicit VA/SA-phase-1/SA-phase-2 arbiters
/// instead of the [`RoundRobin`] default: how a config selecting `matrix` or
/// `islip` (P1-F6) would wire a run.
// One param per Simulation::new component; same justified width as that
// constructor.
#[allow(clippy::too_many_arguments)]
pub fn build_sim_with_allocators(
    width: u16,
    height: u16,
    cfg: SimConfig,
    rate: f64,
    routing: Box<dyn RoutingAlgorithm>,
    va_alloc: Box<dyn Allocator>,
    sa1_alloc: Box<dyn Allocator>,
    sa2_alloc: Box<dyn Allocator>,
) -> Simulation {
    let nodes = width as u32 * height as u32;
    Simulation::new(
        cfg,
        Box::new(Mesh2D::new(width, height)),
        routing,
        Box::new(FirstAvailable),
        va_alloc,
        sa1_alloc,
        sa2_alloc,
        Box::new(Uniform::new(nodes, rate, cfg.packet_size)),
    )
}

/// Build a mesh sim with the routing algorithm named by `routing` key.
pub fn build_sim_named(
    width: u16,
    height: u16,
    cfg: SimConfig,
    rate: f64,
    routing: &str,
) -> Simulation {
    let ctx = plugin_ctx(width, height, cfg, rate);
    let router = builtin_registry()
        .build_routing(routing, &ctx)
        .unwrap_or_else(|| panic!("unknown routing '{routing}'"));
    build_sim_with(width, height, cfg, rate, router)
}

pub fn build_sim(width: u16, height: u16, cfg: SimConfig, rate: f64) -> Simulation {
    build_sim_named(width, height, cfg, rate, "xy")
}

/// Run one offered-load point through warm-up, measurement, and drain phases.
pub fn run_point(p: &RunParams, rate: f64) -> RunPoint {
    let mut sim = build_sim_named(p.width, p.height, p.cfg, rate, &p.routing);
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
