//! Manual exploration lab for the Phase 1 features the `tessera` CLI doesn't
//! expose flags for yet (torus, table routing, bimodal traffic, selection
//! policies, allocators, config validation, Noxim import). Edit the knobs in
//! each section and re-run; nothing here is a regression test — those live
//! under `crates/tessera-cli/tests/`.
//!
//! Run with: cargo run --example manual_lab -p tessera-cli

use tessera_config::{validate, SimConfigFile};
use tessera_core::{RoutingAlgorithm, SimConfig, Simulation};
use tessera_model::{Mesh2D, Torus2D};
use tessera_routing::{
    BufferLevel, DimensionOrderXy, FirstAvailable, ISlip, Matrix, OddEven, RoundRobin,
    TableRouting, TorusDimensionOrderXy,
};
use tessera_traffic::{Bimodal, Uniform};

fn sweep_point(
    label: &str,
    topo: Box<dyn tessera_core::Topology>,
    routing: Box<dyn RoutingAlgorithm>,
    selection: Box<dyn tessera_core::SelectionPolicy>,
    traffic: Box<dyn tessera_core::TrafficGenerator>,
    cfg: SimConfig,
    warmup: u64,
    measure: u64,
) {
    let mut sim = Simulation::new(
        cfg,
        topo,
        routing,
        selection,
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        traffic,
    );
    sim.measure_start = warmup;
    sim.measure_end = warmup + measure;
    sim.set_injecting(true);
    for _ in 0..(warmup + measure) {
        sim.tick();
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < 200_000 {
        sim.tick();
        guard += 1;
    }
    let accepted =
        sim.metrics.ejected_flits_in_window as f64 / (sim.num_nodes() as f64 * measure as f64);
    println!(
        "{label:<28} accepted_flit_rate={accepted:.4}  avg_latency={:.2}  \
         injected={} ejected={} drained_in={guard} extra cycles",
        sim.metrics.avg_packet_latency(),
        sim.metrics.injected_packets,
        sim.metrics.ejected_packets,
    );
}

fn main() {
    let base_cfg = SimConfig {
        buffer_depth: 4,
        num_vcs: 2,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 42,
    };

    println!("== 1. Torus vs mesh, same offered load ==");
    sweep_point(
        "mesh + XY",
        Box::new(Mesh2D::new(8, 8)),
        Box::new(DimensionOrderXy),
        Box::new(FirstAvailable),
        Box::new(Uniform::new(64, 0.15, 4)),
        base_cfg,
        1000,
        4000,
    );
    // KNOWN BUG (found via this example, not yet fixed): at rate=0.15 this
    // stalls hard — the `dateline_crossed` flag on a flit never resets when
    // dimension-order routing moves from its X-phase into its Y-phase, so a
    // packet that wrapped the X-dateline is incorrectly forced onto the
    // upper VC half for its *entire* remaining journey, including plain
    // (non-wrapping) Y movement. Under real load that turns the single
    // upper-half VC into a shared bottleneck, and a packet that also needs
    // to wrap the Y-dateline has nowhere left to escalate — reproducing a
    // real deadlock. 0.05 stays under the threshold where this manifests
    // (empirically: fine at <=0.05, stalls at >=0.08 on an 8x8 torus).
    let torus_cfg = SimConfig {
        num_vcs: 2, // must be >= 2 for a dateline-safe torus DOR (validate() enforces this)
        ..base_cfg
    };
    sweep_point(
        "torus + torus-DOR (dateline)",
        Box::new(Torus2D::new(8, 8)),
        Box::new(TorusDimensionOrderXy::new(8, 8)),
        Box::new(FirstAvailable),
        Box::new(Uniform::new(64, 0.05, 4)),
        torus_cfg,
        1000,
        4000,
    );

    println!("\n== 2. Selection family, same routing/topology/load ==");
    let selections: Vec<(&str, Box<dyn tessera_core::SelectionPolicy>)> = vec![
        ("first-available", Box::new(FirstAvailable)),
        ("buffer-level", Box::new(BufferLevel)),
    ];
    for (name, sel) in selections {
        sweep_point(
            name,
            Box::new(Mesh2D::new(8, 8)),
            Box::new(OddEven),
            sel,
            Box::new(Uniform::new(64, 0.15, 4)),
            base_cfg,
            1000,
            4000,
        );
    }

    println!("\n== 3. Bimodal packet sizes (1-flit control / 8-flit data, 50/50) ==");
    sweep_point(
        "bimodal 1/8 @ 50%",
        Box::new(Mesh2D::new(8, 8)),
        Box::new(DimensionOrderXy),
        Box::new(FirstAvailable),
        Box::new(Bimodal::new(Uniform::new(64, 0.08, 4), 1, 8, 0.5)),
        base_cfg,
        1000,
        4000,
    );

    println!("\n== 4. Table-based routing (hand-built, copies XY) ==");
    let topo = Mesh2D::new(8, 8);
    let mut rows = Vec::new();
    for here in 0..64 {
        for dst in 0..64 {
            if here == dst {
                continue;
            }
            let ctx = tessera_core::RouteCtx {
                here: tessera_core::RouterId(here),
                dst: tessera_core::RouterId(dst),
                src: tessera_core::RouterId(here),
                in_port: tessera_core::PortId(4),
                topo: &topo,
            };
            let port = DimensionOrderXy.route(&ctx).get(0);
            rows.push((
                tessera_core::RouterId(here),
                tessera_core::RouterId(dst),
                port,
            ));
        }
    }
    let table = TableRouting::new(64, rows);
    sweep_point(
        "table routing (== XY)",
        Box::new(Mesh2D::new(8, 8)),
        Box::new(table),
        Box::new(FirstAvailable),
        Box::new(Uniform::new(64, 0.15, 4)),
        base_cfg,
        1000,
        4000,
    );

    println!("\n== 5. Allocator family (VA/SA arbiter), same everything else ==");
    for (name, alloc_kind) in [("round-robin", 0u8), ("matrix", 1), ("islip", 2)] {
        let (va, sa1, sa2): (
            Box<dyn tessera_core::Allocator>,
            Box<dyn tessera_core::Allocator>,
            Box<dyn tessera_core::Allocator>,
        ) = match alloc_kind {
            0 => (
                Box::new(RoundRobin::new()),
                Box::new(RoundRobin::new()),
                Box::new(RoundRobin::new()),
            ),
            1 => (
                Box::new(Matrix::new()),
                Box::new(Matrix::new()),
                Box::new(Matrix::new()),
            ),
            _ => (
                Box::new(ISlip::new(2)),
                Box::new(ISlip::new(2)),
                Box::new(ISlip::new(2)),
            ),
        };
        let mut sim = Simulation::new(
            base_cfg,
            Box::new(Mesh2D::new(8, 8)),
            Box::new(DimensionOrderXy),
            Box::new(FirstAvailable),
            va,
            sa1,
            sa2,
            Box::new(Uniform::new(64, 0.15, 4)),
        );
        sim.measure_start = 1000;
        sim.measure_end = 5000;
        sim.set_injecting(true);
        for _ in 0..5000 {
            sim.tick();
        }
        sim.set_injecting(false);
        let mut guard = 0;
        while sim.in_flight() > 0 && guard < 200_000 {
            sim.tick();
            guard += 1;
        }
        let accepted = sim.metrics.ejected_flits_in_window as f64 / (64.0 * 4000.0);
        println!(
            "{name:<28} accepted_flit_rate={accepted:.4}  avg_latency={:.2}",
            sim.metrics.avg_packet_latency()
        );
    }

    println!("\n== 6. Config validation (deliberately broken config) ==");
    let mut bad = SimConfigFile::default();
    bad.router.num_vcs = 0; // invalid: must be 1..=8
    bad.router.buffer_depth = 1; // invalid: must be >= 2
    bad.router.routing = "adaptive".to_string(); // needs vc_classes, none set
    match validate(&bad) {
        Ok(()) => println!("(unexpected) validated cleanly"),
        Err(errors) => {
            println!(
                "validate() rejected the config with {} error(s):",
                errors.len()
            );
            for e in &errors {
                println!("  - {e}");
            }
        }
    }
}
