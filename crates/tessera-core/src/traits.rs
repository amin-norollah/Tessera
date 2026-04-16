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

/// Picks one output port from the admissible set.
pub trait SelectionPolicy: Send + Sync {
    fn select(&self, cands: &Candidates) -> PortId;
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
