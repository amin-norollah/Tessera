//! Config → engine wiring (OUT-13, OUT-26): a `SimConfigFile` builds a live
//! `Simulation` honoring every component choice. Wiring is proven by an
//! identical per-cycle event trace (every routing/VA/SA decision) against a
//! hand-assembled `Simulation` using the components the config names: a
//! mis-mapped allocator/selection diverges. Aggregate metrics alone can't
//! prove this: under DOR with fixed-size packets, totals like latency sum are
//! largely invariant to which contender an arbiter picks.
//! and every file-backed `TrafficKind` builds and runs from a config on disk
//! with relative paths. Load failures surface as `BuildError`s, not panics.

use std::path::{Path, PathBuf};

use tessera_cli::{
    build_from_config, build_from_path, build_sim_with_allocators, run_configured, run_point,
    BuildError, RunParams,
};
use tessera_config::{
    AllocatorCfg, PacketSizeCfg, SimConfigFile, TopologyCfg, TrafficCfg, TrafficKind, VcClassCfg,
};
use tessera_core::{Event, SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_routing::{BufferLevel, DimensionOrderXy, ISlip, Matrix, RoundRobin, WestFirst};
use tessera_traffic::Uniform;

const WARMUP: u64 = 300;
const MEASURE: u64 = 1500;

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tessera_cli_config_build_test_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn small_config() -> SimConfigFile {
    SimConfigFile {
        warmup: WARMUP,
        measure: MEASURE,
        ..SimConfigFile::default()
    }
}

fn sim_cfg(cfg: &SimConfigFile) -> SimConfig {
    SimConfig {
        buffer_depth: cfg.router.buffer_depth,
        num_vcs: cfg.router.num_vcs,
        router_latency: cfg.router.router_latency,
        link_latency: cfg.router.link_latency,
        credit_delay: cfg.router.credit_delay,
        packet_size: 4,
        seed: cfg.seed,
    }
}

/// Runs `cycles` injecting, then drains; returns the full event trace.
fn drive(sim: &mut Simulation, cycles: u64) -> Vec<Event> {
    sim.record_events(true);
    let mut events = Vec::new();
    sim.set_injecting(true);
    for _ in 0..cycles {
        sim.tick();
        events.extend(sim.take_events());
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < 100_000 {
        sim.tick();
        events.extend(sim.take_events());
        guard += 1;
    }
    assert_eq!(sim.in_flight(), 0, "network must drain");
    events
}

#[test]
fn default_config_reproduces_the_legacy_runner() {
    let cfg = small_config();
    let from_config = run_configured(build_from_config(&cfg, Path::new(".")).unwrap(), 200_000);

    let (width, height) = cfg.topology.dimensions();
    let legacy = run_point(
        &RunParams {
            width,
            height,
            cfg: sim_cfg(&cfg),
            routing: "xy".to_string(),
            warmup: WARMUP,
            measure: MEASURE,
            drain_cap: 200_000,
        },
        0.1,
    );

    assert_eq!(from_config.injected_packets, legacy.injected_packets);
    assert_eq!(from_config.ejected_packets, legacy.ejected_packets);
    assert_eq!(
        from_config.avg_packet_latency.to_bits(),
        legacy.avg_packet_latency.to_bits()
    );
    assert_eq!(
        from_config.accepted_flit_rate.to_bits(),
        legacy.accepted_flit_rate.to_bits()
    );
    assert_eq!(from_config.offered_pkt_rate, 0.1);
    assert_eq!(from_config.offered_flit_rate, 0.4);
}

#[test]
fn allocator_choices_reach_the_engine() {
    let mut cfg = small_config();
    cfg.router.num_vcs = 2;
    cfg.router.allocator_va = AllocatorCfg::ISlip;
    cfg.router.allocator_sa = AllocatorCfg::Matrix;
    cfg.router.alloc_iters = 2;
    cfg.traffic.kind = TrafficKind::Uniform {
        injection_rate: 0.3,
    };
    let cycles = WARMUP + MEASURE;

    let mut built = build_from_config(&cfg, Path::new(".")).unwrap().sim;
    let built_trace = drive(&mut built, cycles);

    let mut manual = build_sim_with_allocators(
        8,
        8,
        sim_cfg(&cfg),
        0.3,
        Box::new(DimensionOrderXy),
        Box::new(ISlip::new(2)),
        Box::new(Matrix::new()),
        Box::new(Matrix::new()),
    );
    assert!(built_trace == drive(&mut manual, cycles), "trace diverged");

    // And the choice matters: the round-robin default makes different
    // arbitration decisions at this load, so the equality isn't vacuous.
    let mut default_alloc = build_sim_with_allocators(
        8,
        8,
        sim_cfg(&cfg),
        0.3,
        Box::new(DimensionOrderXy),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
    );
    assert!(built_trace != drive(&mut default_alloc, cycles));
}

#[test]
fn selection_and_routing_choices_reach_the_engine() {
    let mut cfg = small_config();
    cfg.router.num_vcs = 2;
    cfg.router.routing = "west-first".to_string();
    cfg.router.selection = "buffer-level".to_string();
    cfg.traffic.kind = TrafficKind::Uniform {
        injection_rate: 0.2,
    };
    let cycles = WARMUP + MEASURE;

    let mut built = build_from_config(&cfg, Path::new(".")).unwrap().sim;
    let built_trace = drive(&mut built, cycles);

    let mut manual = Simulation::new(
        sim_cfg(&cfg),
        Box::new(Mesh2D::new(8, 8)),
        Box::new(WestFirst),
        Box::new(BufferLevel),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(Uniform::new(64, 0.2, 4)),
    );
    assert!(built_trace == drive(&mut manual, cycles), "trace diverged");
}

#[test]
fn torus_with_dateline_vcs_builds_and_drains() {
    let mut cfg = small_config();
    cfg.topology = TopologyCfg::Torus2D {
        width: 4,
        height: 4,
    };
    cfg.router.num_vcs = 2;
    cfg.router.dateline = true;
    let mut run = build_from_config(&cfg, Path::new(".")).unwrap();
    assert_eq!(run.nodes, 16);
    drive(&mut run.sim, WARMUP + MEASURE);
    assert!(run.sim.metrics.ejected_packets > 0);
}

#[test]
fn bimodal_packet_sizes_apply_to_rate_traffic() {
    let mut cfg = small_config();
    cfg.traffic.packet_size = PacketSizeCfg::Bimodal {
        control: 1,
        data: 8,
        data_ratio: 0.25,
    };
    let mut run = build_from_config(&cfg, Path::new(".")).unwrap();
    assert_eq!(run.mean_packet_flits, 1.0 + 0.25 * 7.0);
    drive(&mut run.sim, WARMUP + MEASURE);
    let m = &run.sim.metrics;
    let flits_per_packet = m.ejected_flits as f64 / m.ejected_packets as f64;
    assert!(
        (flits_per_packet - 2.75).abs() < 0.3,
        "mean packet size {flits_per_packet}, expected ~2.75"
    );
}

/// Writes `files` into a fresh scratch dir plus a `run.toml` naming them by
/// relative path, then builds through `build_from_path`.
fn build_on_disk(
    name: &str,
    traffic: TrafficKind,
    files: &[(&str, &str)],
) -> Result<tessera_cli::ConfiguredRun, BuildError> {
    let dir = scratch_dir(name);
    for (file, body) in files {
        std::fs::write(dir.join(file), body).unwrap();
    }
    let cfg = SimConfigFile {
        traffic: TrafficCfg {
            kind: traffic,
            packet_size: PacketSizeCfg::Bimodal {
                control: 1,
                data: 4,
                data_ratio: 0.5,
            },
        },
        ..small_config()
    };
    let path = dir.join("run.toml");
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    build_from_path(&path)
}

#[test]
fn table_based_traffic_runs_from_a_config_file() {
    // Only node 0 sends, only to node 5 (5 hops east on the 8x8 mesh).
    let table = "# src dst pir por t_on t_off t_period\n0 5 0.2 0.2 0 100 100\n";
    let mut run = build_on_disk(
        "table",
        TrafficKind::TableBased {
            path: "traffic.txt".to_string(),
        },
        &[("traffic.txt", table)],
    )
    .unwrap();
    drive(&mut run.sim, WARMUP + MEASURE);
    let m = &run.sim.metrics;
    assert!(m.ejected_packets > 100, "{m:?}");
    assert_eq!(m.total_hops, m.ejected_flits * 5, "every flit took 0 -> 5");
}

#[test]
fn netrace_traffic_runs_from_a_config_file() {
    let trace = "version 1\n0 0 5 control -\n1 1 5 control -\n2 5 10 data 0,1\n";
    let mut run = build_on_disk(
        "netrace",
        TrafficKind::Netrace {
            path: "trace.tni".to_string(),
            region: None,
            node_mapping: tessera_config::NodeMappingCfg::Identity,
        },
        &[("trace.tni", trace)],
    )
    .unwrap();
    assert!(run.provenance.as_deref().unwrap().contains("trace.tni"));
    drive(&mut run.sim, WARMUP + MEASURE);
    assert_eq!(run.sim.metrics.ejected_packets, 3);
    // Bimodal sizing maps classes: two 1-flit controls + one 4-flit data.
    assert_eq!(run.sim.metrics.ejected_flits, 6);
}

#[test]
fn synfull_traffic_runs_from_a_config_file() {
    let model = r#"
        name = "cfg-test"
        initial_state = "a"

        [[states]]
        name = "a"
        inject_prob = 0.1
        size_class = "control"
        dest = { kind = "uniform" }
        next = [ { state = "a", prob = 0.8 }, { state = "b", prob = 0.2 } ]

        [[states]]
        name = "b"
        inject_prob = 0.3
        size_class = "data"
        dest = { kind = "hotspot", node = 0, share = 0.5 }
        next = [ { state = "a", prob = 1.0 } ]
    "#;
    let run = build_on_disk(
        "synfull",
        TrafficKind::SynFull {
            model_path: "model.toml".to_string(),
        },
        &[("model.toml", model)],
    )
    .unwrap();
    assert!(run.provenance.as_deref().unwrap().contains("model.toml"));
    let point = run_configured(run, 200_000);
    assert!(point.ejected_packets > 0);
    assert_eq!(point.ejected_packets, point.injected_packets);
    assert!(
        point.offered_pkt_rate > 0.0,
        "measured offered load reported"
    );
}

#[test]
fn load_failures_are_actionable_errors() {
    let missing = build_on_disk(
        "missing",
        TrafficKind::Netrace {
            path: "nope.tni".to_string(),
            region: None,
            node_mapping: tessera_config::NodeMappingCfg::Identity,
        },
        &[],
    );
    assert!(matches!(missing, Err(BuildError::Io { .. })));

    let cyclic = build_on_disk(
        "cyclic",
        TrafficKind::Netrace {
            path: "trace.tni".to_string(),
            region: None,
            node_mapping: tessera_config::NodeMappingCfg::Identity,
        },
        &[("trace.tni", "version 1\n0 0 1 control 1\n1 1 0 control 0\n")],
    );
    assert!(matches!(cyclic, Err(BuildError::Netrace { .. })));

    let out_of_range = build_on_disk(
        "table_range",
        TrafficKind::TableBased {
            path: "t.txt".to_string(),
        },
        &[("t.txt", "0 99 0.1 0.1 0 10 10\n")],
    );
    let err = out_of_range.err().expect("node 99 is off an 8x8 mesh");
    assert!(err.to_string().contains("t.txt"), "names the file: {err}");

    let bad_model = build_on_disk(
        "bad_model",
        TrafficKind::SynFull {
            model_path: "m.toml".to_string(),
        },
        &[(
            "m.toml",
            "name = \"x\"\ninitial_state = \"a\"\nstates = []\n",
        )],
    );
    assert!(matches!(bad_model, Err(BuildError::SynFull { .. })));
}

#[test]
fn unsupported_component_choices_are_rejected() {
    let build = |cfg: &SimConfigFile| build_from_config(cfg, Path::new(".")).err();

    let mut cfg = small_config();
    cfg.router.selection = "telepathic".to_string();
    let err = build(&cfg).unwrap();
    assert!(matches!(err, BuildError::UnknownSelection { .. }));
    assert!(err.to_string().contains("buffer-level"), "lists known keys");

    let mut cfg = small_config();
    cfg.router.routing = "zigzag".to_string();
    assert!(matches!(
        build(&cfg).unwrap(),
        BuildError::UnknownRouting { .. }
    ));

    let mut cfg = small_config();
    cfg.topology = TopologyCfg::Torus2D {
        width: 4,
        height: 4,
    };
    cfg.router.routing = "west-first".to_string();
    assert!(matches!(
        build(&cfg).unwrap(),
        BuildError::RoutingUnsupportedOnTorus(_)
    ));

    let mut cfg = small_config();
    cfg.router.routing = "adaptive".to_string();
    cfg.router.num_vcs = 2;
    cfg.router.vc_classes = Some(VcClassCfg {
        escape: 1,
        adaptive: 1,
    });
    assert!(matches!(
        build(&cfg).unwrap(),
        BuildError::VcClassesUnsupported
    ));

    let mut cfg = small_config();
    cfg.router.buffer_depth = 1;
    cfg.router.alloc_iters = 0;
    match build(&cfg).unwrap() {
        BuildError::Invalid(errs) => assert_eq!(errs.len(), 2, "{errs:?}"),
        other => panic!("expected Invalid, got {other}"),
    }
}
