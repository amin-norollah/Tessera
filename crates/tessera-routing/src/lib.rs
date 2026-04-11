//! Routing and selection behind the [`RoutingAlgorithm`] / [`SelectionPolicy`]
//! contracts.
//!
//! Phase 1 ships the deterministic dimension-order routers (XY, XYZ) and the
//! partially-adaptive turn-model family (west-first, north-last,
//! negative-first). Each turn model returns the productive output ports
//! permitted by its turn set; deadlock-freedom on a mesh with a single VC
//! follows from the turn model (Glass & Ni). Odd-even (which needs a
//! parity-aware, source-column-dependent selection) and fully-adaptive +
//! escape-VC (Duato) routing arrive in a later increment.

use tessera_core::{
    Candidates, Coord, PortId, PortLabel, RouteCtx, RoutingAlgorithm, SelectionPolicy,
};

fn opposite(l: PortLabel) -> PortLabel {
    match l {
        PortLabel::XPlus => PortLabel::XMinus,
        PortLabel::XMinus => PortLabel::XPlus,
        PortLabel::YPlus => PortLabel::YMinus,
        PortLabel::YMinus => PortLabel::YPlus,
        PortLabel::ZPlus => PortLabel::ZMinus,
        PortLabel::ZMinus => PortLabel::ZPlus,
        PortLabel::Local => PortLabel::Local,
    }
}

/// Productive directions (2D) that reduce the Manhattan distance to `dst`.
fn productive_2d(here: Coord, dst: Coord) -> ([PortLabel; 2], usize) {
    let mut d = [PortLabel::Local; 2];
    let mut n = 0;
    if dst.x > here.x {
        d[n] = PortLabel::XPlus;
        n += 1;
    } else if dst.x < here.x {
        d[n] = PortLabel::XMinus;
        n += 1;
    }
    if dst.y > here.y {
        d[n] = PortLabel::YPlus;
        n += 1;
    } else if dst.y < here.y {
        d[n] = PortLabel::YMinus;
        n += 1;
    }
    (d, n)
}

/// The dimension-order single-port route for XY/XYZ.
fn dor_label(here: Coord, dst: Coord) -> PortLabel {
    if dst.x > here.x {
        PortLabel::XPlus
    } else if dst.x < here.x {
        PortLabel::XMinus
    } else if dst.y > here.y {
        PortLabel::YPlus
    } else if dst.y < here.y {
        PortLabel::YMinus
    } else if dst.z > here.z {
        PortLabel::ZPlus
    } else if dst.z < here.z {
        PortLabel::ZMinus
    } else {
        PortLabel::Local
    }
}

fn label_to_port(ctx: &RouteCtx, label: PortLabel) -> PortId {
    ctx.topo
        .port_for_label(ctx.here, label)
        .expect("a productive/minimal route stays within topology bounds")
}

/// Dimension-order XY routing (2D). Deadlock-free on a mesh with one VC.
pub struct DimensionOrderXy;

impl RoutingAlgorithm for DimensionOrderXy {
    fn route(&self, ctx: &RouteCtx) -> Candidates {
        let here = ctx.topo.coord(ctx.here);
        let dst = ctx.topo.coord(ctx.dst);
        Candidates::single(label_to_port(ctx, dor_label(here, dst)))
    }
}

/// Dimension-order XYZ routing (3D). Exhausts X, then Y, then Z. On a 2D mesh
/// (z == 0) it behaves exactly like XY.
pub struct DimensionOrderXyz;

impl RoutingAlgorithm for DimensionOrderXyz {
    fn route(&self, ctx: &RouteCtx) -> Candidates {
        let here = ctx.topo.coord(ctx.here);
        let dst = ctx.topo.coord(ctx.dst);
        Candidates::single(label_to_port(ctx, dor_label(here, dst)))
    }
}

#[derive(Clone, Copy)]
enum TurnModel {
    WestFirst,
    NorthLast,
    NegativeFirst,
}

/// Whether the turn from travel direction `from` to `to` is permitted. `from`
/// is `None` for a freshly injected flit (no prior direction).
fn turn_allowed(model: TurnModel, from: Option<PortLabel>, to: PortLabel) -> bool {
    use PortLabel::*;
    let Some(from) = from else {
        return true;
    };
    match model {
        // Forbid turns that end going west after a vertical move.
        TurnModel::WestFirst => !(matches!(from, YPlus | YMinus) && to == XMinus),
        // Forbid turning out of north (take north last).
        TurnModel::NorthLast => !(from == YPlus && matches!(to, XPlus | XMinus)),
        // Forbid positive->negative turns (route negatives first).
        TurnModel::NegativeFirst => {
            !((from == XPlus && to == YMinus) || (from == YPlus && to == XMinus))
        }
    }
}

/// Ordering key so [`FirstAvailable`] realizes each model's "first/last"
/// discipline (lower = higher priority = tried first).
fn priority(model: TurnModel, to: PortLabel) -> u8 {
    use PortLabel::*;
    match model {
        TurnModel::WestFirst => (to != XMinus) as u8,
        TurnModel::NorthLast => (to == YPlus) as u8,
        TurnModel::NegativeFirst => !matches!(to, XMinus | YMinus) as u8,
    }
}

fn turn_route(ctx: &RouteCtx, model: TurnModel) -> Candidates {
    let here = ctx.topo.coord(ctx.here);
    let dst = ctx.topo.coord(ctx.dst);
    if here.x == dst.x && here.y == dst.y {
        return Candidates::single(label_to_port(ctx, PortLabel::Local));
    }
    let from = match ctx.topo.label_for_port(ctx.here, ctx.in_port) {
        PortLabel::Local => None,
        lbl => Some(opposite(lbl)),
    };

    let (dirs, n) = productive_2d(here, dst);
    let mut chosen = [PortLabel::Local; 2];
    let mut c = 0;
    for &to in dirs.iter().take(n) {
        if turn_allowed(model, from, to) {
            chosen[c] = to;
            c += 1;
        }
    }
    // A connected turn model always leaves at least one legal productive move on
    // a legal minimal path; keep progressing if a model edge case empties it.
    if c == 0 {
        debug_assert!(false, "turn model left no legal productive move");
        chosen[0] = dirs[0];
        c = 1;
    }
    if c == 2 && priority(model, chosen[0]) > priority(model, chosen[1]) {
        chosen.swap(0, 1);
    }

    let mut cands = Candidates::new();
    for &label in chosen.iter().take(c) {
        cands.push(label_to_port(ctx, label));
    }
    cands
}

macro_rules! turn_router {
    ($name:ident, $model:expr, $doc:literal) => {
        #[doc = $doc]
        pub struct $name;
        impl RoutingAlgorithm for $name {
            fn route(&self, ctx: &RouteCtx) -> Candidates {
                turn_route(ctx, $model)
            }
            fn is_deterministic(&self) -> bool {
                false
            }
        }
    };
}

turn_router!(
    WestFirst,
    TurnModel::WestFirst,
    "West-first turn-model routing."
);
turn_router!(
    NorthLast,
    TurnModel::NorthLast,
    "North-last turn-model routing."
);
turn_router!(
    NegativeFirst,
    TurnModel::NegativeFirst,
    "Negative-first turn-model routing."
);

/// Picks the first admissible port. With deterministic routing there is exactly
/// one candidate; with a turn model it takes the model's highest-priority legal
/// productive port.
pub struct FirstAvailable;

impl SelectionPolicy for FirstAvailable {
    fn select(&self, cands: &Candidates) -> PortId {
        debug_assert!(!cands.is_empty(), "routing must admit at least one port");
        cands.get(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::{RouterId, Topology};
    use tessera_model::{Mesh2D, EAST, LOCAL, NORTH, SOUTH, WEST};

    fn ctx<'a>(topo: &'a Mesh2D, here: u32, dst: u32, in_port: u8) -> RouteCtx<'a> {
        RouteCtx {
            here: RouterId(here),
            dst: RouterId(dst),
            in_port: PortId(in_port),
            topo,
        }
    }

    #[test]
    fn xy_is_deterministic_and_productive() {
        let m = Mesh2D::new(4, 4);
        // node 0 (0,0) -> node 5 (1,1): XY goes east first.
        let c = DimensionOrderXy.route(&ctx(&m, 0, 5, LOCAL));
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(0).0, EAST);
        // arrived: routes to local.
        let c = DimensionOrderXy.route(&ctx(&m, 5, 5, WEST));
        assert_eq!(c.get(0).0, LOCAL);
    }

    #[test]
    fn west_first_takes_west_before_turning() {
        let m = Mesh2D::new(4, 4);
        // (2,0) injected, dst (0,1) = northwest: must offer west first.
        let here = m.router_at(tessera_core::Coord::new(2, 0, 0)).unwrap().0;
        let dst = m.router_at(tessera_core::Coord::new(0, 1, 0)).unwrap().0;
        let c = WestFirst.route(&ctx(&m, here, dst, LOCAL));
        assert_eq!(c.get(0).0, WEST, "west offered first");
    }

    #[test]
    fn turn_models_only_emit_productive_ports() {
        let m = Mesh2D::new(6, 6);
        // From an interior node arriving from various ports, every candidate a
        // turn model emits must be a productive minimal move.
        let here = m.router_at(tessera_core::Coord::new(3, 3, 0)).unwrap().0;
        let dst = m.router_at(tessera_core::Coord::new(5, 5, 0)).unwrap().0; // NE
        for (router, name) in [
            (&WestFirst as &dyn RoutingAlgorithm, "wf"),
            (&NorthLast, "nl"),
            (&NegativeFirst, "nf"),
        ] {
            for in_port in [EAST, WEST, NORTH, SOUTH, LOCAL] {
                let c = router.route(&ctx(&m, here, dst, in_port));
                assert!(!c.is_empty(), "{name} empty on in_port {in_port}");
                for i in 0..c.len() {
                    let p = c.get(i).0;
                    // NE destination => only EAST or NORTH are productive.
                    assert!(p == EAST || p == NORTH, "{name} non-productive port {p}");
                }
            }
        }
    }
}
