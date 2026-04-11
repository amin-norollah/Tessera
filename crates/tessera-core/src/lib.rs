//! `tessera-core` — the engine spine: index handles, the flit arena, SoA input
//! buffers, deterministic per-node RNG, the extension trait contracts, and the
//! double-buffered BSP simulation kernel.
//!
//! This crate sits at the bottom of the dependency graph. Concrete topologies,
//! routing/selection algorithms, and traffic generators live in the higher
//! crates and are injected into [`Simulation`] as trait objects.

pub mod buffer;
pub mod flit;
pub mod ids;
pub mod rng;
pub mod sim;
pub mod traits;

pub use buffer::BufferPool;
pub use flit::{FlitArena, FlitHeader, FlitKind};
pub use ids::{Coord, FlitId, LinkId, PacketId, PeId, PortId, RouterId, VcId};
pub use rng::{NodeRng, RngStreams};
pub use sim::{Metrics, SimConfig, Simulation, VcState};
pub use traits::{
    Candidates, PacketDesc, PortLabel, RouteCtx, RoutingAlgorithm, SelectionPolicy, Topology,
    TrafficGenerator,
};
