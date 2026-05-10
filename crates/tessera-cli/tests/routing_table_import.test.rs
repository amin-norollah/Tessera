//! End-to-end Noxim routing-table import (P1-F15/G1.3): parse a Noxim-format
//! routing table, build a `TableRouting` from the parsed rows, and run a
//! real simulation to confirm the imported table actually drives delivery.

use tessera_config::parse_routing_table;
use tessera_core::{PortId, RouteCtx, RouterId, RoutingAlgorithm, SimConfig, Simulation, Topology};
use tessera_model::Mesh2D;
use tessera_routing::{DimensionOrderXy, FirstAvailable, RoundRobin, TableRouting};
use tessera_traffic::Uniform;

/// Renders XY routing's own decisions as a Noxim-format `src dst port` table
/// text, standing in for a routing table exported by an external tool.
fn xy_as_noxim_table(topo: &Mesh2D) -> String {
    let n = topo.num_routers();
    let mut text = String::new();
    for here in 0..n {
        for dst in 0..n {
            if here == dst {
                continue;
            }
            let ctx = RouteCtx {
                here: RouterId(here),
                dst: RouterId(dst),
                src: RouterId(here),
                in_port: PortId(topo.local_port().0),
                topo,
            };
            let port = DimensionOrderXy.route(&ctx).get(0);
            text.push_str(&format!("{here} {dst} {}\n", port.0));
        }
    }
    text
}

#[test]
fn imported_table_drives_a_real_run() {
    let topo = Mesh2D::new(6, 6);
    let n = topo.num_routers();
    let text = xy_as_noxim_table(&topo);

    let rows = parse_routing_table(&text).expect("well-formed table");
    let table = TableRouting::new(n, rows.into_iter().map(|r| (r.src, r.dst, r.port)));

    let cfg = SimConfig {
        buffer_depth: 4,
        num_vcs: 1,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 5,
    };
    let mut sim = Simulation::new(
        cfg,
        Box::new(Mesh2D::new(6, 6)),
        Box::new(table),
        Box::new(FirstAvailable),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(Uniform::new(n, 0.1, 4)),
    );
    sim.set_injecting(true);
    for _ in 0..3000 {
        sim.tick();
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < 100_000 {
        sim.tick();
        guard += 1;
    }

    assert_eq!(sim.in_flight(), 0, "imported table did not drain");
    assert_eq!(
        sim.metrics.injected_packets, sim.metrics.ejected_packets,
        "imported table lost packets"
    );
    assert!(sim.metrics.ejected_packets > 0, "delivered nothing");
}
