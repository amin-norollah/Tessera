//! Concrete NoC topologies on the abstract [`Topology`] contract. Phase 0 ships
//! the 2D mesh; torus/3D and beyond arrive in later phases without touching the
//! router or kernel.

mod mesh2d;

pub use mesh2d::{Mesh2D, EAST, LOCAL, NORTH, SOUTH, WEST};
