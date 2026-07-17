//! Routing and selection behind the [`RoutingAlgorithm`] / [`SelectionPolicy`]
//! contracts.
//!
//! Phase 1 ships the deterministic dimension-order routers (XY, XYZ), the
//! partially-adaptive turn-model family (west-first, north-last,
//! negative-first, odd-even), and a torus-aware dimension-order router. Each
//! turn model returns the productive output ports permitted by its turn set;
//! deadlock-freedom on a mesh with a single VC follows from the turn model
//! (Glass & Ni for west-first/north-last/negative-first; Chiu for odd-even).
//! Fully-adaptive + escape-VC (Duato) routing arrives in a later increment.

use tessera_core::{Candidates, Coord, PortId, PortLabel, RouteCtx, RoutingAlgorithm};

mod alloc;
mod selection;
mod table;

pub use alloc::{ISlip, Matrix, RoundRobin};
pub use selection::{BufferLevel, FirstAvailable, Random, RoundRobinSelection};
pub use table::TableRouting;

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

/// Whether moving in the increasing (`+`) direction around a `size`-node ring
/// is the shorter-or-tied path from `here` to `dst`; `None` if `here == dst`
/// (no movement needed in this dimension).
fn shorter_ring_plus(here: u16, dst: u16, size: u16) -> Option<bool> {
    if here == dst {
        return None;
    }
    let forward = (dst as i32 - here as i32).rem_euclid(size as i32);
    let backward = size as i32 - forward;
    Some(forward <= backward)
}

/// Dimension-order routing on a 2D torus: each dimension wraps through the
/// dateline whenever that is the shorter path, exhausting X before Y. The
/// abstract [`Topology`] trait has no notion of ring size, so: unlike the
/// mesh routers above, which derive everything from `ctx.topo`: this needs
/// the torus's actual width/height at construction time.
pub struct TorusDimensionOrderXy {
    width: u16,
    height: u16,
}

impl TorusDimensionOrderXy {
    pub fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

impl RoutingAlgorithm for TorusDimensionOrderXy {
    fn route(&self, ctx: &RouteCtx) -> Candidates {
        let here = ctx.topo.coord(ctx.here);
        let dst = ctx.topo.coord(ctx.dst);
        let label = if let Some(plus) = shorter_ring_plus(here.x, dst.x, self.width) {
            if plus {
                PortLabel::XPlus
            } else {
                PortLabel::XMinus
            }
        } else if let Some(plus) = shorter_ring_plus(here.y, dst.y, self.height) {
            if plus {
                PortLabel::YPlus
            } else {
                PortLabel::YMinus
            }
        } else {
            PortLabel::Local
        };
        Candidates::single(label_to_port(ctx, label))
    }
}

#[derive(Clone, Copy)]
enum TurnModel {
    WestFirst,
    NorthLast,
    NegativeFirst,
}

/// Whether the turn from travel direction `from` to `to` is permitted.
/// `from` is `None` for a freshly injected flit (no prior direction).
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
    // Only reachable by querying a history a real run never produces (e.g. a
    // conformance/CDG checker probing every `(in_port, dst)` pair, including
    // ones no legal multi-hop path leads to): the priority filter below
    // guarantees that whenever a mandatory direction is still pending, it is
    // taken at the *first* opportunity, so a later hop can never arrive here
    // with that direction both mandatory and already forbidden. Fall back
    // rather than return nothing, since `RoutingAlgorithm::route` must always
    // return at least one port.
    if c == 0 {
        chosen[0] = dirs[0];
        c = 1;
    }
    if c == 2 {
        if priority(model, chosen[0]) > priority(model, chosen[1]) {
            chosen.swap(0, 1);
        }
        // A strict priority difference is mandatory ordering, not a
        // preference: these turn models are safe under arbitrary selection
        // only because the deferrable direction is never actually offered as
        // a free choice against the must-go-now one (a selection policy that
        // ignores priority: buffer-level, round-robin, random: would
        // sometimes pick the deferrable candidate and strand the mandatory
        // turn later, deadlocking). Only a genuine tie (both candidates
        // equally preferred: e.g. west-first doesn't care which of two
        // *unrestricted* directions comes first) is left as a real choice.
        if priority(model, chosen[0]) < priority(model, chosen[1]) {
            c = 1;
        }
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

/// Whether a router in column `here_x` may take the turn from `from` to
/// `to`, per Chiu's odd-even rule: EN/ES turns forbidden at even columns,
/// NW/SW turns forbidden at odd columns. Unlike [`turn_allowed`]'s models,
/// this restriction is genuinely position-dependent (not a fixed priority),
/// which is what [`odd_even_offer`] has to account for.
fn odd_even_turn_allowed(here_x: u16, from: Option<PortLabel>, to: PortLabel) -> bool {
    use PortLabel::*;
    let Some(from) = from else {
        return true;
    };
    let even_column = here_x.is_multiple_of(2);
    !matches!(
        (from, to, even_column),
        (XPlus, YPlus, true)
            | (XPlus, YMinus, true)
            | (YPlus, XMinus, false)
            | (YMinus, XMinus, false)
    )
}

/// Whether `to` is safe to offer right now: legal per
/// [`odd_even_turn_allowed`], *and* guaranteed not to strand the packet
/// later. Continuing an axis is only ever forbidden as a followup to a turn
/// (never as a straight continuation), so the sole danger is deferring the
/// *other* axis until it becomes mandatory at a column where it's forbidden.
/// Moving along Y never changes the column that gates the eventual X turn,
/// so that side needs no lookahead (`here.x` already is the column the X
/// turn will happen at); moving along X does change the column, so that
/// side must check legality at `dst.x`: wherever the X axis will actually
/// finish, not just one step ahead.
fn odd_even_offer(here: Coord, dst: Coord, from: Option<PortLabel>, to: PortLabel) -> bool {
    use PortLabel::*;
    if !odd_even_turn_allowed(here.x, from, to) {
        return false;
    }
    match to {
        XPlus | XMinus if here.y != dst.y => {
            let needed_y = if dst.y > here.y { YPlus } else { YMinus };
            odd_even_turn_allowed(dst.x, Some(to), needed_y)
        }
        YPlus | YMinus if here.x != dst.x => {
            let needed_x = if dst.x > here.x { XPlus } else { XMinus };
            odd_even_turn_allowed(here.x, Some(to), needed_x)
        }
        _ => true,
    }
}

/// Odd-even turn-model routing (Chiu 2000). More adaptive than a fixed-
/// priority turn model (west-first et al.): both candidates, when both are
/// offered, are a genuine tie: safe for any [`SelectionPolicy`] to pick
/// between, unlike the mandatory-first turns above.
pub struct OddEven;

impl RoutingAlgorithm for OddEven {
    fn route(&self, ctx: &RouteCtx) -> Candidates {
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
        let mut cands = Candidates::new();
        for &to in dirs.iter().take(n) {
            if odd_even_offer(here, dst, from, to) {
                cands.push(label_to_port(ctx, to));
            }
        }
        if cands.is_empty() {
            // Reachable only via a history no legal multi-hop path produces
            // (see `turn_route`'s equivalent fallback): `odd_even_offer`'s
            // safety analysis guarantees a real run never empties this set.
            cands.push(label_to_port(ctx, dirs[0]));
        }
        cands
    }

    fn is_deterministic(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::{RouterId, Topology};
    use tessera_model::{Mesh2D, Torus2D, EAST, LOCAL, NORTH, SOUTH, WEST};

    fn ctx<'a>(topo: &'a Mesh2D, here: u32, dst: u32, in_port: u8) -> RouteCtx<'a> {
        RouteCtx {
            here: RouterId(here),
            dst: RouterId(dst),
            src: RouterId(here),
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

    #[test]
    fn ring_direction_picks_the_shorter_side() {
        // 4-node ring: 3 -> 1 is shorter going + (wrap: 3->0->1, 2 hops) than
        // - (3->2->1, 2 hops is a tie, so + wins); 3 -> 0 is shorter going +
        // (1 hop via wrap) than - (3 hops).
        assert_eq!(shorter_ring_plus(3, 0, 4), Some(true), "adjacent via wrap");
        assert_eq!(
            shorter_ring_plus(0, 3, 4),
            Some(false),
            "adjacent via wrap, other way"
        );
        assert_eq!(shorter_ring_plus(1, 1, 4), None, "no movement needed");
    }

    #[test]
    fn torus_dor_wraps_through_the_dateline_when_shorter() {
        let t = Torus2D::new(4, 4);
        let router = TorusDimensionOrderXy::new(4, 4);
        // (3,0) -> (0,0): wrapping east 1 hop beats going west 3 hops.
        let here = t.router_at(tessera_core::Coord::new(3, 0, 0)).unwrap().0;
        let dst = t.router_at(tessera_core::Coord::new(0, 0, 0)).unwrap().0;
        let c = router.route(&RouteCtx {
            here: RouterId(here),
            dst: RouterId(dst),
            src: RouterId(here),
            in_port: PortId(LOCAL),
            topo: &t,
        });
        assert_eq!(c.len(), 1);
        assert_eq!(
            c.get(0).0,
            EAST,
            "should wrap east rather than go west 3 hops"
        );
        assert!(
            t.is_dateline(RouterId(here), c.get(0)),
            "that hop crosses the dateline"
        );
    }

    #[test]
    fn torus_dor_drains_cleanly_with_dateline_vcs() {
        // A full RC->VA->SA->ST run with real traffic must deliver everything
        // and drain, proving the dateline VC-half restriction in
        // `Simulation::stage_vc_alloc` doesn't strand packets (P1-F12).
        use tessera_core::{PacketDesc, SimConfig, Simulation, TrafficGenerator};

        struct AllPairs;
        impl TrafficGenerator for AllPairs {
            fn on_tick(
                &self,
                cycle: u64,
                node: RouterId,
                rng: &mut tessera_core::NodeRng,
                out: &mut Vec<PacketDesc>,
            ) {
                if cycle < 200 && rng.next_f64() < 0.2 {
                    let dst = RouterId((node.0 + 7) % 16);
                    out.push(PacketDesc { dst, size: 4 });
                }
            }
        }

        let cfg = SimConfig {
            buffer_depth: 4,
            num_vcs: 2,
            router_latency: 1,
            link_latency: 1,
            credit_delay: 1,
            packet_size: 4,
            seed: 21,
        };
        let mut sim = Simulation::new(
            cfg,
            Box::new(Torus2D::new(4, 4)),
            Box::new(TorusDimensionOrderXy::new(4, 4)),
            Box::new(FirstAvailable),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(AllPairs),
        );
        sim.set_injecting(true);
        for _ in 0..1000 {
            sim.tick();
        }
        sim.set_injecting(false);
        let mut guard = 0;
        while sim.in_flight() > 0 && guard < 50_000 {
            sim.tick();
            guard += 1;
        }
        assert_eq!(
            sim.in_flight(),
            0,
            "torus DOR did not drain (possible deadlock)"
        );
        assert_eq!(sim.metrics.injected_packets, sim.metrics.ejected_packets);
        assert!(sim.metrics.injected_packets > 0);
    }
}
