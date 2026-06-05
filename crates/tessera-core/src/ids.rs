//! Copy index-handles and coordinates.
//!
//! Every entity is referenced by a small `Copy` newtype over `u32`/`u8`: an
//! index into a contiguous store, never a pointer. This is what lets SoA
//! storage, snapshotting, and parallelism coexist (see `INITIAL_PRD` §20.2).

macro_rules! id_newtype {
    ($(#[$m:meta])* $name:ident, $inner:ty) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        #[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
        pub struct $name(pub $inner);

        impl $name {
            #[inline]
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_newtype!(/// A router (and its co-located tile) in the network.
    RouterId, u32);
id_newtype!(/// A processing element; in Phase 0 there is one per router.
    PeId, u32);
id_newtype!(/// A directed link between two router ports.
    LinkId, u32);
id_newtype!(/// A router port. Directions are topology-defined, not hard-coded.
    PortId, u8);
id_newtype!(/// A virtual channel within a port. Phase 0 uses a single VC.
    VcId, u8);
id_newtype!(/// A flit, i.e. an index into the flit arena.
    FlitId, u32);
id_newtype!(/// A packet.
    PacketId, u32);

/// A network coordinate. Meaning is topology-defined; for meshes/tori these are
/// grid indices.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Coord {
    pub x: u16,
    pub y: u16,
    pub z: u16,
}

impl Coord {
    pub fn new(x: u16, y: u16, z: u16) -> Self {
        Self { x, y, z }
    }
}
