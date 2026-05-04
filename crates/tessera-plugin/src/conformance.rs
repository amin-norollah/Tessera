//! Contract checks any registered component must pass. These are the executable
//! form of the trait invariants frozen at [`crate::PLUGIN_API_VERSION`]; the
//! plugin conformance test suite (P2-F7) runs them against every built-in and
//! against third-party plugins.

use tessera_core::{
    Candidates, CongestionView, Coord, NodeRng, PortId, RngStreams, RouteCtx, RouterId,
    RoutingAlgorithm, SelectionPolicy, Topology,
};

use crate::{PluginCtx, Registry};

fn manhattan(a: Coord, b: Coord) -> u32 {
    let d = |x: u16, y: u16| (x.max(y) - x.min(y)) as u32;
    d(a.x, b.x) + d(a.y, b.y) + d(a.z, b.z)
}

fn same(a: &Candidates, b: &Candidates) -> bool {
    a.len() == b.len() && (0..a.len()).all(|i| a.get(i).0 == b.get(i).0)
}

/// Check a routing algorithm against its contract on `topo`:
/// non-empty admissible set, deterministic for a fixed input, local port only
/// on arrival, and every candidate a legal minimal (productive) move otherwise.
///
/// Routes are probed from the local (injection) port, which is always a legal
/// arrival direction; multi-hop legality under a turn model's own prior hops is
/// covered by the full-drain property test.
pub fn check_routing(router: &dyn RoutingAlgorithm, topo: &dyn Topology) -> Result<(), String> {
    let n = topo.num_routers();
    let local = topo.local_port();
    for h in 0..n {
        let here = RouterId(h);
        let here_c = topo.coord(here);
        for d in 0..n {
            let dst = RouterId(d);
            let ctx = RouteCtx {
                here,
                dst,
                src: here,
                in_port: local,
                topo,
            };

            let cands = router.route(&ctx);
            if cands.is_empty() {
                return Err(format!("empty candidate set at here={h} dst={d}"));
            }
            if !same(&cands, &router.route(&ctx)) {
                return Err(format!("non-deterministic route at here={h} dst={d}"));
            }

            if h == d {
                if cands.len() != 1 || cands.get(0).0 != local.0 {
                    return Err(format!(
                        "arrived (here==dst={h}) must route to the local port"
                    ));
                }
                continue;
            }

            let dist_here = manhattan(here_c, topo.coord(dst));
            for i in 0..cands.len() {
                let port = cands.get(i);
                if port.0 == local.0 {
                    return Err(format!(
                        "local port offered for non-arrived flit at here={h} dst={d}"
                    ));
                }
                match topo.adjacent(here, port) {
                    None => {
                        return Err(format!(
                            "candidate port {} at router {h} has no neighbor",
                            port.0
                        ))
                    }
                    Some((neighbor, _)) => {
                        if manhattan(topo.coord(neighbor), topo.coord(dst)) >= dist_here {
                            return Err(format!(
                                "non-minimal candidate port {} at here={h} dst={d}",
                                port.0
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Run [`check_routing`] against every routing algorithm registered in `reg`,
/// reporting the first that fails.
pub fn check_all_routing(
    reg: &Registry,
    topo: &dyn Topology,
    ctx: &PluginCtx,
) -> Result<(), String> {
    for name in reg.routing_names() {
        let router = reg
            .build_routing(name, ctx)
            .ok_or_else(|| format!("registry could not build '{name}'"))?;
        check_routing(router.as_ref(), topo).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(())
}

/// A congestion view with no downstream state, for conformance checks that
/// have no real router to read credits from.
struct NoCongestion;

impl CongestionView for NoCongestion {
    fn free_slots(&self, _port: PortId) -> u16 {
        0
    }
}

/// Check a selection policy: it returns a port drawn from the admissible set.
pub fn check_selection(policy: &mut dyn SelectionPolicy, cands: &Candidates) -> Result<(), String> {
    if cands.is_empty() {
        return Ok(());
    }
    let mut rng: NodeRng = RngStreams::new(1).router(0);
    let chosen = policy.select(cands, &NoCongestion, &mut rng);
    let in_set = (0..cands.len()).any(|i| cands.get(i).0 == chosen.0);
    if in_set {
        Ok(())
    } else {
        Err(format!(
            "selection returned port {} not in the admissible set",
            chosen.0
        ))
    }
}
