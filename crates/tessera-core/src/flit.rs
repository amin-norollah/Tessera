//! Flit data model and the pooled flit arena.
//!
//! Header/payload are split so the hot path moves a 4-byte [`FlitId`] handle
//! rather than the fat struct (this fixes PAT-Noxim's per-hop value copy). The
//! arena is the single owner of flit storage for the whole simulation and
//! recycles ids, so steady state does no per-flit heap allocation.

use crate::ids::{FlitId, PacketId, RouterId, VcId};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlitKind {
    Head,
    Body,
    Tail,
    HeadTail,
}

impl FlitKind {
    /// Head flits carry routing info and trigger route computation + VC alloc.
    #[inline]
    pub fn needs_route(self) -> bool {
        matches!(self, FlitKind::Head | FlitKind::HeadTail)
    }

    /// Tail flits release the input VC and the output-port reservation.
    #[inline]
    pub fn frees_worm(self) -> bool {
        matches!(self, FlitKind::Tail | FlitKind::HeadTail)
    }
}

/// The small, cache-friendly, POD part of a flit — the only part that moves
/// through the datapath.
#[derive(Clone, Copy, Debug)]
pub struct FlitHeader {
    pub packet: PacketId,
    pub kind: FlitKind,
    pub src: RouterId,
    pub dst: RouterId,
    /// Flit index within its packet.
    pub seq: u16,
    /// Current virtual channel.
    pub vc: VcId,
    pub hops: u16,
    /// Cycle the packet was generated at the source (for end-to-end latency).
    pub inject_cycle: u64,
    /// Earliest cycle this flit may leave its current buffer, i.e. arrival +
    /// router latency. Models the per-hop router pipeline delay.
    pub eligible_cycle: u64,
    /// Set once this packet has crossed a torus dateline link
    /// ([`Topology::is_dateline`](crate::traits::Topology::is_dateline));
    /// from then on VC allocation restricts it to the upper VC half, the
    /// standard 2-VC dateline deadlock-avoidance scheme (P1-F12).
    pub dateline_crossed: bool,
}

/// Pooled flit storage. Ids are recycled via the free list; no per-flit heap
/// allocation on the steady-state path.
#[derive(Default)]
pub struct FlitArena {
    headers: Vec<FlitHeader>,
    free: Vec<FlitId>,
}

impl FlitArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alloc(&mut self, header: FlitHeader) -> FlitId {
        if let Some(id) = self.free.pop() {
            self.headers[id.index()] = header;
            id
        } else {
            let id = FlitId(self.headers.len() as u32);
            self.headers.push(header);
            id
        }
    }

    #[inline]
    pub fn get(&self, id: FlitId) -> &FlitHeader {
        &self.headers[id.index()]
    }

    #[inline]
    pub fn get_mut(&mut self, id: FlitId) -> &mut FlitHeader {
        &mut self.headers[id.index()]
    }

    pub fn free(&mut self, id: FlitId) {
        self.free.push(id);
    }

    /// Number of flit slots currently checked out (live + not-yet-recycled).
    pub fn live(&self) -> usize {
        self.headers.len() - self.free.len()
    }
}
