//! Table-based routing (P1-F9): a precomputed `(here, dst) -> port` lookup,
//! typically imported from a legacy Noxim routing table
//! (`tessera_config::parse_routing_table`) or a proven-acyclic scheme
//! (up*/down* trees, a spanning tree over an irregular topology). This is
//! *table-based*, per-hop routing: each router independently looks up its
//! own next hop. It is not *source* routing (the whole path fixed once at
//! injection and carried in the packet); that would need a `FlitHeader`
//! change to carry the remaining path and is a follow-up, not implemented
//! here.
//!
//! `TableRouting` only executes the lookup; it doesn't verify the table is
//! acyclic. An imported table is accepted as user-asserted deadlock-free,
//! matching Noxim's own guarantee (`docs/phases/PHASE_1_PRD.md`).

use tessera_core::{Candidates, PortId, RouteCtx, RouterId, RoutingAlgorithm};

const NO_ENTRY: u8 = u8::MAX;

/// A `(here, dst) -> port` routing table over an `n`-router network.
pub struct TableRouting {
    n: u32,
    port: Vec<u8>,
}

impl TableRouting {
    /// Build from `(here, dst, port)` rows. Panics if a row references a
    /// router `>= n`: a one-time construction-time check, not a per-lookup
    /// cost.
    pub fn new(n: u32, rows: impl IntoIterator<Item = (RouterId, RouterId, PortId)>) -> Self {
        let mut port = vec![NO_ENTRY; n as usize * n as usize];
        for (here, dst, p) in rows {
            assert!(
                here.0 < n && dst.0 < n,
                "routing table row references a router >= {n}"
            );
            port[here.0 as usize * n as usize + dst.0 as usize] = p.0;
        }
        Self { n, port }
    }
}

impl RoutingAlgorithm for TableRouting {
    fn route(&self, ctx: &RouteCtx) -> Candidates {
        if ctx.here == ctx.dst {
            return Candidates::single(ctx.topo.local_port());
        }
        let idx = ctx.here.0 as usize * self.n as usize + ctx.dst.0 as usize;
        let port = self.port[idx];
        assert_ne!(
            port, NO_ENTRY,
            "no routing table entry for (here={}, dst={})",
            ctx.here.0, ctx.dst.0
        );
        Candidates::single(PortId(port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::Topology;
    use tessera_model::Mesh2D;

    #[test]
    fn looks_up_the_configured_port() {
        let table = TableRouting::new(4, [(RouterId(0), RouterId(3), PortId(2))]);
        let topo = Mesh2D::new(2, 2);
        let ctx = RouteCtx {
            here: RouterId(0),
            dst: RouterId(3),
            src: RouterId(0),
            in_port: PortId(4),
            topo: &topo,
        };
        assert_eq!(table.route(&ctx).get(0).0, 2);
    }

    #[test]
    fn routes_to_local_on_arrival() {
        let table = TableRouting::new(4, []);
        let topo = Mesh2D::new(2, 2);
        let ctx = RouteCtx {
            here: RouterId(1),
            dst: RouterId(1),
            src: RouterId(0),
            in_port: PortId(4),
            topo: &topo,
        };
        assert_eq!(table.route(&ctx).get(0).0, topo.local_port().0);
    }

    #[test]
    #[should_panic(expected = "no routing table entry")]
    fn panics_on_a_missing_entry() {
        let table = TableRouting::new(4, []);
        let topo = Mesh2D::new(2, 2);
        let ctx = RouteCtx {
            here: RouterId(0),
            dst: RouterId(3),
            src: RouterId(0),
            in_port: PortId(4),
            topo: &topo,
        };
        table.route(&ctx);
    }

    /// A table built by copying `DimensionOrderXy`'s own decisions must
    /// behave identically to XY: the mechanism (lookup vs. computed rule)
    /// shouldn't matter to the routing outcome.
    #[test]
    fn a_table_copying_xy_matches_xy_everywhere() {
        use crate::DimensionOrderXy;
        let topo = Mesh2D::new(5, 4);
        let n = topo.num_routers();
        let mut rows = Vec::new();
        for here in 0..n {
            for dst in 0..n {
                let ctx = RouteCtx {
                    here: RouterId(here),
                    dst: RouterId(dst),
                    src: RouterId(here),
                    in_port: PortId(4),
                    topo: &topo,
                };
                let port = DimensionOrderXy.route(&ctx).get(0);
                rows.push((RouterId(here), RouterId(dst), port));
            }
        }
        let table = TableRouting::new(n, rows);
        for here in 0..n {
            for dst in 0..n {
                let ctx = RouteCtx {
                    here: RouterId(here),
                    dst: RouterId(dst),
                    src: RouterId(here),
                    in_port: PortId(4),
                    topo: &topo,
                };
                assert_eq!(
                    table.route(&ctx).get(0).0,
                    DimensionOrderXy.route(&ctx).get(0).0,
                    "here={here} dst={dst}"
                );
            }
        }
    }
}
