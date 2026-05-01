//! Concrete NoC topologies on the abstract [`Topology`] contract: the 2D mesh
//! (Phase 0) and 2D torus with dateline wraparound (Phase 1); 3D and beyond
//! arrive in later phases without touching the router or kernel.

mod mesh2d;
mod torus2d;

pub use mesh2d::{Mesh2D, EAST, LOCAL, NORTH, SOUTH, WEST};
pub use torus2d::Torus2D;
