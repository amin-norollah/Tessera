//! The extension contracts the engine composes.
//!
//! These live in `tessera-core` so the [`crate::sim::Simulation`] can hold them
//! as trait objects while concrete implementations live in the higher crates
//! (`tessera-model`, `tessera-routing`, `tessera-traffic`).
//!
//! **Plugin API v1 (frozen at Phase 2, `tessera_plugin::PLUGIN_API_VERSION`):**
//! [`RoutingAlgorithm`], [`SelectionPolicy`], and [`TrafficGenerator`] are
//! frozen — additive changes only within the 1.x line. Each documents the
//! invariants a conforming implementation must uphold (see
//! `tessera_plugin::conformance`). The `Allocator` and `StatisticsCollector`
//! contracts join the freeze once their engine implementations land.

use crate::ids::{Coord, PortId, RouterId};
use crate::rng::NodeRng;

/// A conventional direction label a topology attaches to a port. Routing
/// algorithms that reason about cardinal directions use these; the router
/// itself never assumes a mesh.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PortLabel {
    XPlus,
    XMinus,
    YPlus,
    YMinus,
    ZPlus,
    ZMinus,
    Local,
}

/// Which dimension a [`PortLabel`] moves along; `None` for [`PortLabel::Local`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    X,
    Y,
    Z,
}

pub fn axis_of(label: PortLabel) -> Option<Axis> {
    match label {
        PortLabel::XPlus | PortLabel::XMinus => Some(Axis::X),
        PortLabel::YPlus | PortLabel::YMinus => Some(Axis::Y),
        PortLabel::ZPlus | PortLabel::ZMinus => Some(Axis::Z),
        PortLabel::Local => None,
    }
}

/// Network structure as data: nodes, their ports, and how ports connect.
pub trait Topology: Send + Sync {
    fn num_routers(&self) -> u32;
    /// Port count per router (uniform in Phase 0).
    fn port_count(&self) -> u8;
    /// The reserved local (injection/ejection) port.
    fn local_port(&self) -> PortId;
    fn coord(&self, r: RouterId) -> Coord;
    fn router_at(&self, c: Coord) -> Option<RouterId>;

    /// The peer `(router, port)` sharing the full-duplex link on port `p` of
    /// `r`. Because links are symmetric this is both where `r`'s output on `p`
    /// delivers and the upstream that feeds `r`'s input on `p`. `None` at a
    /// boundary or for the local port.
    fn adjacent(&self, r: RouterId, p: PortId) -> Option<(RouterId, PortId)>;

    fn port_for_label(&self, r: RouterId, label: PortLabel) -> Option<PortId>;
    fn label_for_port(&self, r: RouterId, p: PortId) -> PortLabel;

    /// Whether port `p` of router `r` carries a dateline link: a wraparound
    /// edge that a torus's dimension-order routing must cross onto a
    /// dedicated upper-VC-half class to stay deadlock-free (P1-F12). Additive
    /// default `false`; only wraparound topologies (e.g. a torus) need to
    /// override it. A dateline is a topology property, not a routing one, so
    /// routing algorithms stay topology-agnostic.
    fn is_dateline(&self, r: RouterId, p: PortId) -> bool {
        let _ = (r, p);
        false
    }
}

/// The productive output ports a routing function admits. Small and stack-
/// allocated to keep route computation allocation-free.
#[derive(Clone, Copy, Debug)]
pub struct Candidates {
    ports: [u8; 8],
    len: u8,
}

impl Default for Candidates {
    fn default() -> Self {
        Self::new()
    }
}

impl Candidates {
    pub fn new() -> Self {
        Self {
            ports: [0; 8],
            len: 0,
        }
    }

    pub fn single(p: PortId) -> Self {
        let mut c = Self::new();
        c.push(p);
        c
    }

    pub fn push(&mut self, p: PortId) {
        assert!(
            (self.len as usize) < self.ports.len(),
            "too many route candidates"
        );
        self.ports[self.len as usize] = p.0;
        self.len += 1;
    }

    pub fn get(&self, i: usize) -> PortId {
        PortId(self.ports[i])
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Context handed to a routing function for one head flit.
pub struct RouteCtx<'a> {
    pub here: RouterId,
    pub dst: RouterId,
    /// The packet's injection node. Most routing families ignore it (they
    /// only need `here`/`dst`), but source-dependent ones need it: odd-even
    /// turn restrictions key off the *source* column's parity, and table/
    /// source routing looks up `(src, dst)` directly.
    pub src: RouterId,
    pub in_port: PortId,
    pub topo: &'a dyn Topology,
}

/// Computes the admissible (productive) output ports for a packet. Returns at
/// least one port; a single port for deterministic routing.
pub trait RoutingAlgorithm: Send + Sync {
    fn route(&self, ctx: &RouteCtx) -> Candidates;
    /// Whether the same input always yields the same single port (used later
    /// for VC-dependency checks).
    fn is_deterministic(&self) -> bool {
        true
    }
}

/// Read-only per-port congestion state a [`SelectionPolicy`] may consult —
/// backed by the router's live credit state, not a snapshot copy.
pub trait CongestionView {
    /// Total free downstream buffer slots (summed across VCs) on output
    /// `port`.
    fn free_slots(&self, port: PortId) -> u16;
}

/// Picks one output port from the admissible set.
///
/// Revised from the signature frozen at Phase 2 (`tessera_plugin::
/// PLUGIN_API_VERSION` 1: `select(&self, cands: &Candidates) -> PortId`):
/// that freeze predates this Phase 1 backfill, which discovered the
/// congestion-aware and stateful members of P1-F11's selection family
/// (buffer-level, round-robin, random) are unimplementable without seeing
/// live credit state or carrying pointer/RNG state between calls. Both gaps
/// are closed here (`congestion`, `&mut self`, `rng`); a stable v1 should be
/// re-declared against *this* shape once Phase 1 lands, not the pre-backfill
/// one.
pub trait SelectionPolicy: Send + Sync {
    fn select(
        &mut self,
        cands: &Candidates,
        congestion: &dyn CongestionView,
        rng: &mut NodeRng,
    ) -> PortId;
}

/// A packet a PE wants to inject.
#[derive(Clone, Copy, Debug)]
pub struct PacketDesc {
    pub dst: RouterId,
    pub size: u16,
}

/// Generates traffic at a PE. Called once per cycle per node while injection is
/// enabled; pushes packet descriptors to inject this cycle.
pub trait TrafficGenerator: Send + Sync {
    fn on_tick(&self, cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>);
}

/// A bid: `requester` wants `resource`. A requester may appear more than once
/// (bidding for several resources it would equally accept, e.g. VC
/// allocation over a pool of free output VCs); the allocator grants at most
/// one resource per requester and at most one requester per resource.
///
/// Ids are flat indices whose meaning is entirely the caller's (e.g.
/// `port*vcs + vc`); an [`Allocator`] never interprets them, only compares
/// and orders them. **Ids must stay stable across cycles** for round-robin/
/// iSLIP fairness pointers to mean anything — reassigning what id N refers to
/// from one cycle to the next defeats the fairness discipline silently.
#[derive(Clone, Copy, Debug)]
pub struct AllocRequest {
    pub requester: u32,
    pub resource: u32,
}

/// The outcome of one [`Allocator::allocate`] call. The caller `reset`s it to
/// the requester/resource counts for that call, then reads grants back out;
/// the backing storage is reused across calls (`reset` clears without
/// shrinking capacity), so steady-state arbitration allocates no memory.
#[derive(Default)]
pub struct GrantSet {
    resource_winner: Vec<i64>,
    requester_grant: Vec<i64>,
}

const NO_GRANT: i64 = -1;

impl GrantSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear all grants and size for `num_requesters` requesters and
    /// `num_resources` resources.
    pub fn reset(&mut self, num_requesters: usize, num_resources: usize) {
        self.requester_grant.clear();
        self.requester_grant.resize(num_requesters, NO_GRANT);
        self.resource_winner.clear();
        self.resource_winner.resize(num_resources, NO_GRANT);
    }

    pub fn num_requesters(&self) -> usize {
        self.requester_grant.len()
    }

    pub fn num_resources(&self) -> usize {
        self.resource_winner.len()
    }

    /// Record that `requester` won `resource`. An [`Allocator`] impl must
    /// call this at most once per requester and at most once per resource
    /// within one `allocate` call.
    pub fn grant(&mut self, requester: u32, resource: u32) {
        self.requester_grant[requester as usize] = resource as i64;
        self.resource_winner[resource as usize] = requester as i64;
    }

    pub fn requester_grant(&self, requester: u32) -> Option<u32> {
        let r = self.requester_grant[requester as usize];
        (r >= 0).then_some(r as u32)
    }

    pub fn resource_winner(&self, resource: u32) -> Option<u32> {
        let r = self.resource_winner[resource as usize];
        (r >= 0).then_some(r as u32)
    }
}

/// Bipartite matching between requesters and resources — the shared
/// primitive behind VC allocation and both phases of switch allocation
/// (`INITIAL_PRD.md` §20.7: "used for VA and SA"). Concrete arbiters
/// (round-robin, matrix, iSLIP) live in `tessera-routing`.
///
/// Deliberately `&mut self` rather than the indicative `&self` in
/// `INITIAL_PRD.md` §20: a fair arbiter carries per-resource/per-requester
/// pointer state that must advance between calls, and `&self` would force
/// that into interior mutability for no benefit (this trait is unstable
/// pre-Phase-2-freeze, per this file's own doc comment, so the deviation is
/// free to make now).
pub trait Allocator: Send + Sync {
    /// Match `reqs` into `grants`. `grants` arrives already [`GrantSet::reset`]
    /// to this call's requester/resource counts.
    fn allocate(&mut self, reqs: &[AllocRequest], grants: &mut GrantSet);
}
