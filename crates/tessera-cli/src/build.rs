//! Config-driven run assembly: turns a validated [`SimConfigFile`] into a
//! live [`Simulation`], mapping every component choice onto its engine impl
//! (OUT-13: topology, routing, selection, VA/SA allocators, `alloc_iters`;
//! OUT-26: every [`TrafficKind`] plus [`PacketSizeCfg`]).
//!
//! **Why a separate path, not the plugin [`Registry`](tessera_plugin::Registry).**
//! The registry's v1 factories are infallible (`Fn(&PluginCtx) -> Box<dyn _>`)
//! and frozen at `PLUGIN_API_VERSION` 1. File-backed traffic (Noxim tables,
//! Netrace traces, SynFull models) is inherently fallible (I/O, parse, DAG
//! validation). Rather than break the frozen factory signature or panic on a
//! bad path, routing/selection stay resolved by key through the registry and
//! file-backed sources are built here, where every failure surfaces as a
//! [`BuildError`] naming the offending config field or file.
//!
//! Relative file paths in the config resolve against `base_dir` (normally the
//! config file's own directory), so a config and its traces travel together.

use std::path::{Path, PathBuf};

use thiserror::Error;

use tessera_config::{
    parse_traffic_table, validate, AllocatorCfg, ConfigError, NodeMappingCfg, NoximImportError,
    PacketSizeCfg, SimConfigFile, TopologyCfg, TrafficKind,
};
use tessera_core::{
    Allocator, RoutingAlgorithm, SelectionPolicy, SimConfig, Simulation, Topology, TrafficGenerator,
};
use tessera_model::{Mesh2D, Torus2D};
use tessera_plugin::{builtin_registry, PluginCtx};
use tessera_routing::{ISlip, Matrix, RoundRobin, TorusDimensionOrderXy};
use tessera_stats::RunPoint;
use tessera_traffic::netrace::{
    MessageSizing, Netrace, NetraceError, NetraceTrace, NodeMapping, TraceProvenance,
};
use tessera_traffic::synfull::{ModelProvenance, SynFull, SynFullError, SynFullModel};
use tessera_traffic::table::{TableRow, TableTraffic, TableTrafficError};
use tessera_traffic::{Bimodal, Uniform};

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    // TOML errors are large; boxed so every `Result<_, BuildError>` stays
    // small (clippy::result_large_err).
    #[error("{}: not a valid Tessera config: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("invalid config:{}", bullet_list(.0))]
    Invalid(Vec<ConfigError>),

    #[error("router.routing = '{key}' is not a registered routing algorithm (known: {known})")]
    UnknownRouting { key: String, known: String },

    #[error("router.selection = '{key}' is not a registered selection policy (known: {known})")]
    UnknownSelection { key: String, known: String },

    #[error(
        "router.routing = '{0}' is not supported on topology torus2d; only 'xy' (torus \
         dimension-order routing over dateline VCs) is"
    )]
    RoutingUnsupportedOnTorus(String),

    #[error(
        "router.vc_classes: escape-VC (Duato) partitioning is not implemented in the engine \
         yet (docs/OUTSTANDING_PRD.md OUT-14); remove vc_classes"
    )]
    VcClassesUnsupported,

    #[error("traffic table {}: {source}", path.display())]
    TrafficTableSyntax {
        path: PathBuf,
        #[source]
        source: NoximImportError,
    },

    #[error("traffic table {}: {source}", path.display())]
    TrafficTable {
        path: PathBuf,
        #[source]
        source: TableTrafficError,
    },

    #[error("netrace trace {}: {source}", path.display())]
    Netrace {
        path: PathBuf,
        #[source]
        source: NetraceError,
    },

    #[error("synfull model {}: {source}", path.display())]
    SynFull {
        path: PathBuf,
        #[source]
        source: Box<SynFullError>,
    },
}

fn bullet_list(errors: &[ConfigError]) -> String {
    errors.iter().map(|e| format!("\n  - {e}")).collect()
}

/// A [`Simulation`] assembled from a config, plus what the harness needs to
/// drive and report it.
pub struct ConfiguredRun {
    pub sim: Simulation,
    pub nodes: u32,
    /// Grid dimensions (routers are numbered row-major).
    pub width: u16,
    pub height: u16,
    pub warmup: u64,
    pub measure: u64,
    /// The configured offered load (packets/node/cycle) for rate-driven
    /// traffic (`uniform`); `None` for table/trace/model traffic, whose
    /// offered load is reported as measured instead.
    pub offered_rate: Option<f64>,
    /// Mean flits per packet implied by `traffic.packet_size`.
    pub mean_packet_flits: f64,
    /// Trace/model provenance stamp (P3.5-F7), for file-backed traffic.
    pub provenance: Option<String>,
}

/// Read and parse a config file (no validation; [`build_from_config`] does
/// that).
pub fn load_config(path: &Path) -> Result<SimConfigFile, BuildError> {
    let text = read(path)?;
    toml::from_str(&text).map_err(|source| BuildError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

/// Validate `cfg` and assemble its [`Simulation`]. Relative traffic file
/// paths resolve against `base_dir`.
pub fn build_from_config(
    cfg: &SimConfigFile,
    base_dir: &Path,
) -> Result<ConfiguredRun, BuildError> {
    validate(cfg).map_err(BuildError::Invalid)?;
    if cfg.router.vc_classes.is_some() {
        return Err(BuildError::VcClassesUnsupported);
    }

    let (width, height) = cfg.topology.dimensions();
    let nodes = width as u32 * height as u32;
    let (fixed_or_data, mean_packet_flits) = match cfg.traffic.packet_size {
        PacketSizeCfg::Fixed { size } => (size, size as f64),
        PacketSizeCfg::Bimodal {
            control,
            data,
            data_ratio,
        } => (
            data,
            control as f64 + data_ratio * (data as f64 - control as f64),
        ),
    };
    let offered_rate = match cfg.traffic.kind {
        TrafficKind::Uniform { injection_rate } => Some(injection_rate),
        _ => None,
    };

    let sim_cfg = SimConfig {
        buffer_depth: cfg.router.buffer_depth,
        num_vcs: cfg.router.num_vcs,
        router_latency: cfg.router.router_latency,
        link_latency: cfg.router.link_latency,
        credit_delay: cfg.router.credit_delay,
        packet_size: fixed_or_data,
        seed: cfg.seed,
    };
    let ctx = PluginCtx {
        num_nodes: nodes,
        num_vcs: cfg.router.num_vcs,
        packet_size: fixed_or_data,
        injection_rate: offered_rate.unwrap_or(0.0),
    };

    let (topo, routing) = build_topology_and_routing(cfg, &ctx)?;
    let selection = build_selection(&cfg.router.selection, &ctx)?;
    let iters = cfg.router.alloc_iters;
    let va = build_allocator(cfg.router.allocator_va, iters);
    let sa1 = build_allocator(cfg.router.allocator_sa, iters);
    let sa2 = build_allocator(cfg.router.allocator_sa, iters);
    let (traffic, provenance) = build_traffic(cfg, nodes, base_dir)?;

    let sim = Simulation::new(sim_cfg, topo, routing, selection, va, sa1, sa2, traffic);
    Ok(ConfiguredRun {
        sim,
        nodes,
        width,
        height,
        warmup: cfg.warmup,
        measure: cfg.measure,
        offered_rate,
        mean_packet_flits,
        provenance,
    })
}

/// [`load_config`] + [`build_from_config`], resolving traffic paths against
/// the config file's directory.
pub fn build_from_path(path: &Path) -> Result<ConfiguredRun, BuildError> {
    let cfg = load_config(path)?;
    let base = path.parent().unwrap_or(Path::new("."));
    build_from_config(&cfg, base)
}

/// Drive a configured run through warm-up, measurement, and drain (the same
/// harness as [`crate::run_point`]).
pub fn run_configured(run: ConfiguredRun, drain_cap: u64) -> RunPoint {
    run_configured_with(run, drain_cap, |_| {})
}

/// [`run_configured`], calling `on_tick` with the simulation after every tick
/// (warm-up, measurement and drain alike): the hook an
/// [`Observer`](crate::observe::Observer) samples through. It only reads, so
/// the run's result is unchanged.
pub fn run_configured_with(
    mut run: ConfiguredRun,
    drain_cap: u64,
    mut on_tick: impl FnMut(&Simulation),
) -> RunPoint {
    let sim = &mut run.sim;
    sim.measure_start = run.warmup;
    sim.measure_end = run.warmup + run.measure;

    sim.set_injecting(true);
    for _ in 0..(run.warmup + run.measure) {
        sim.tick();
        on_tick(sim);
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < drain_cap {
        sim.tick();
        on_tick(sim);
        guard += 1;
    }

    let nodes = run.nodes as u64;
    let mut point = RunPoint::from_metrics(&sim.metrics, nodes, run.measure, 1, 0.0);
    match run.offered_rate {
        Some(rate) => {
            point.offered_pkt_rate = rate;
            point.offered_flit_rate = rate * run.mean_packet_flits;
        }
        None => {
            // Trace/model traffic has no configured rate: report what the
            // sources actually injected over the injecting period.
            let denom = (nodes * (run.warmup + run.measure).max(1)) as f64;
            point.offered_pkt_rate = sim.metrics.injected_packets as f64 / denom;
            point.offered_flit_rate = sim.metrics.injected_flits as f64 / denom;
        }
    }
    point
}

type TopologyAndRouting = (Box<dyn Topology>, Box<dyn RoutingAlgorithm>);

fn build_topology_and_routing(
    cfg: &SimConfigFile,
    ctx: &PluginCtx,
) -> Result<TopologyAndRouting, BuildError> {
    let key = &cfg.router.routing;
    match cfg.topology {
        TopologyCfg::Mesh2D { width, height } => {
            let registry = builtin_registry();
            let routing =
                registry
                    .build_routing(key, ctx)
                    .ok_or_else(|| BuildError::UnknownRouting {
                        key: key.clone(),
                        known: registry.routing_names().join(", "),
                    })?;
            Ok((Box::new(Mesh2D::new(width, height)), routing))
        }
        // The registry's routing keys are mesh algorithms; on a torus, `xy`
        // means the wraparound-aware DOR router (validation has already
        // required dateline VCs for it).
        TopologyCfg::Torus2D { width, height } => {
            if key != "xy" {
                return Err(BuildError::RoutingUnsupportedOnTorus(key.clone()));
            }
            Ok((
                Box::new(Torus2D::new(width, height)),
                Box::new(TorusDimensionOrderXy::new(width, height)),
            ))
        }
    }
}

fn build_selection(key: &str, ctx: &PluginCtx) -> Result<Box<dyn SelectionPolicy>, BuildError> {
    let registry = builtin_registry();
    registry
        .build_selection(key, ctx)
        .ok_or_else(|| BuildError::UnknownSelection {
            key: key.to_string(),
            known: registry.selection_names().join(", "),
        })
}

/// `alloc_iters` only parameterizes iSLIP; validation rejects `> 1` unless
/// some phase uses it.
fn build_allocator(choice: AllocatorCfg, iters: u8) -> Box<dyn Allocator> {
    match choice {
        AllocatorCfg::RoundRobin => Box::new(RoundRobin::new()),
        AllocatorCfg::Matrix => Box::new(Matrix::new()),
        AllocatorCfg::ISlip => Box::new(ISlip::new(iters)),
    }
}

/// A built generator plus its provenance stamp (file-backed traffic only).
type BuiltTraffic = (Box<dyn TrafficGenerator>, Option<String>);

fn build_traffic(
    cfg: &SimConfigFile,
    nodes: u32,
    base_dir: &Path,
) -> Result<BuiltTraffic, BuildError> {
    let packet_size = cfg.traffic.packet_size;
    match &cfg.traffic.kind {
        TrafficKind::Uniform { injection_rate } => {
            let size = base_size(packet_size);
            Ok((
                sized(Uniform::new(nodes, *injection_rate, size), packet_size),
                None,
            ))
        }
        TrafficKind::TableBased { path } => {
            let path = resolve(base_dir, path);
            let rows = parse_traffic_table(&read(&path)?).map_err(|source| {
                BuildError::TrafficTableSyntax {
                    path: path.clone(),
                    source,
                }
            })?;
            let rows = rows.into_iter().map(|r| TableRow {
                src: r.src,
                dst: r.dst,
                pir: r.pir,
                por: r.por,
                t_on: r.t_on,
                t_off: r.t_off,
                t_period: r.t_period,
            });
            let table =
                TableTraffic::new(nodes, rows, base_size(packet_size)).map_err(|source| {
                    BuildError::TrafficTable {
                        path: path.clone(),
                        source,
                    }
                })?;
            Ok((sized(table, packet_size), None))
        }
        TrafficKind::Netrace {
            path,
            region,
            node_mapping,
        } => {
            let path = resolve(base_dir, path);
            let netrace_err = |source| BuildError::Netrace {
                path: path.clone(),
                source,
            };
            let mut trace = NetraceTrace::parse(&read(&path)?).map_err(netrace_err)?;
            if let Some((start, end)) = *region {
                trace = trace.select_region(start, end);
            }
            let mapping = match node_mapping {
                NodeMappingCfg::Identity => NodeMapping::Identity,
                NodeMappingCfg::Modulo => NodeMapping::Modulo,
            };
            let provenance = TraceProvenance {
                source: path.display().to_string(),
                region: *region,
                node_mapping: mapping,
            };
            let stamp = provenance.stamp();
            let gen = Netrace::load(&trace, nodes, mapping, sizing(packet_size), provenance)
                .map_err(netrace_err)?;
            Ok((Box::new(gen), Some(stamp)))
        }
        TrafficKind::SynFull { model_path } => {
            let path = resolve(base_dir, model_path);
            let model = SynFullModel::from_toml(&read(&path)?, nodes).map_err(|source| {
                BuildError::SynFull {
                    path: path.clone(),
                    source: Box::new(source),
                }
            })?;
            let provenance = ModelProvenance {
                source: path.display().to_string(),
            };
            let stamp = provenance.stamp();
            let gen = SynFull::new(model, nodes, sizing(packet_size), provenance);
            Ok((Box::new(gen), Some(stamp)))
        }
    }
}

/// The per-packet size a rate-driven generator is built with; a bimodal
/// wrapper then redraws each packet's size.
fn base_size(ps: PacketSizeCfg) -> u16 {
    match ps {
        PacketSizeCfg::Fixed { size } => size,
        PacketSizeCfg::Bimodal { data, .. } => data,
    }
}

/// Apply `traffic.packet_size` to a generator that emits fixed-size packets.
fn sized<G: TrafficGenerator + 'static>(gen: G, ps: PacketSizeCfg) -> Box<dyn TrafficGenerator> {
    match ps {
        PacketSizeCfg::Fixed { .. } => Box::new(gen),
        PacketSizeCfg::Bimodal {
            control,
            data,
            data_ratio,
        } => Box::new(Bimodal::new(gen, control, data, data_ratio)),
    }
}

/// Trace/model messages carry their own control/data class, so for them
/// `packet_size` only sets each class's flit count: `fixed` sizes both
/// classes alike, `bimodal` maps control/data directly (its `data_ratio` is
/// unused: the trace, not a coin flip, decides each message's class).
fn sizing(ps: PacketSizeCfg) -> MessageSizing {
    match ps {
        PacketSizeCfg::Fixed { size } => MessageSizing {
            control_flits: size,
            data_flits: size,
        },
        PacketSizeCfg::Bimodal { control, data, .. } => MessageSizing {
            control_flits: control,
            data_flits: data,
        },
    }
}

fn resolve(base_dir: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

fn read(path: &Path) -> Result<String, BuildError> {
    std::fs::read_to_string(path).map_err(|source| BuildError::Io {
        path: path.to_path_buf(),
        source,
    })
}
