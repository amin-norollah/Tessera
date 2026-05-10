//! Phase 2 community gate (G2.1): a routing algorithm defined entirely outside
//! the engine core is registered by string key and runs in a real simulation,
//! delivering all traffic — without any change to the core, model, or kernel.

use tessera_cli::build_sim_with;
use tessera_core::{Candidates, PortLabel, RouteCtx, RoutingAlgorithm, SimConfig};
use tessera_plugin::{builtin_registry, PluginCtx};

/// A third-party YX (y-then-x dimension-order) router. Lives here, not in the
/// core or the routing crate.
struct YxRouting;

impl RoutingAlgorithm for YxRouting {
    fn route(&self, c: &RouteCtx) -> Candidates {
        let here = c.topo.coord(c.here);
        let dst = c.topo.coord(c.dst);
        let label = if dst.y > here.y {
            PortLabel::YPlus
        } else if dst.y < here.y {
            PortLabel::YMinus
        } else if dst.x > here.x {
            PortLabel::XPlus
        } else if dst.x < here.x {
            PortLabel::XMinus
        } else {
            PortLabel::Local
        };
        Candidates::single(c.topo.port_for_label(c.here, label).unwrap())
    }
}

#[test]
fn external_plugin_registers_and_runs() {
    let cfg = SimConfig {
        buffer_depth: 4,
        num_vcs: 1,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 4,
    };

    // Register the out-of-tree router under a new key on top of the built-ins.
    let mut reg = builtin_registry();
    reg.register_routing(
        "yx",
        Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(YxRouting) }),
    );
    assert!(reg.has_routing("yx"));

    let ctx = PluginCtx {
        num_nodes: 64,
        num_vcs: 1,
        packet_size: 4,
        injection_rate: 0.08,
    };
    let router = reg.build_routing("yx", &ctx).expect("resolved by key");

    let mut sim = build_sim_with(8, 8, cfg, 0.08, router);
    sim.set_injecting(true);
    for _ in 0..4000 {
        sim.tick();
    }
    sim.set_injecting(false);
    let mut guard = 0;
    while sim.in_flight() > 0 && guard < 200_000 {
        sim.tick();
        guard += 1;
    }

    assert_eq!(sim.in_flight(), 0, "external plugin routed to a full drain");
    assert_eq!(sim.metrics.injected_packets, sim.metrics.ejected_packets);
    assert!(sim.metrics.ejected_packets > 0);
}
