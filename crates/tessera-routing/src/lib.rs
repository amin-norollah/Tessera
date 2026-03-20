//! Routing and selection behind the [`RoutingAlgorithm`] / [`SelectionPolicy`]
//! contracts. Phase 0 ships deterministic dimension-order XY plus a trivial
//! first-available selection; the turn-model/adaptive/table family lands in
//! Phase 1.

use tessera_core::{Candidates, PortId, PortLabel, RouteCtx, RoutingAlgorithm, SelectionPolicy};

/// Dimension-order XY routing: exhaust the X dimension, then Y. Deadlock-free on
/// a mesh with a single VC.
pub struct DimensionOrderXy;

impl RoutingAlgorithm for DimensionOrderXy {
    fn route(&self, ctx: &RouteCtx) -> Candidates {
        let here = ctx.topo.coord(ctx.here);
        let dst = ctx.topo.coord(ctx.dst);
        let label = if dst.x > here.x {
            PortLabel::XPlus
        } else if dst.x < here.x {
            PortLabel::XMinus
        } else if dst.y > here.y {
            PortLabel::YPlus
        } else if dst.y < here.y {
            PortLabel::YMinus
        } else {
            PortLabel::Local
        };
        let port = ctx
            .topo
            .port_for_label(ctx.here, label)
            .expect("minimal XY route stays within mesh bounds");
        Candidates::single(port)
    }
}

/// Picks the first admissible port. With deterministic routing there is exactly
/// one candidate, so this is the identity choice.
pub struct FirstAvailable;

impl SelectionPolicy for FirstAvailable {
    fn select(&self, cands: &Candidates) -> PortId {
        debug_assert!(!cands.is_empty(), "routing must admit at least one port");
        cands.get(0)
    }
}
