//! Per-cycle observable events (P4-F4): the columnar trace `tessera-record`
//! streams to Arrow/Parquet and diffs bit-for-bit as the replay-equivalence
//! oracle (G4.1). Plain data, no dependencies here — `tessera-core` stays
//! dependency-free; serialization lives in `tessera-record`.
//!
//! [`crate::sim::Simulation::record_events`] turns collection on; each stage
//! kernel pushes into `Simulation`'s event buffer only when it is `Some`, so
//! the non-recording path costs one well-predicted branch and no allocation.

/// One observable occurrence during a cycle. Every variant that names a
/// `packet` lets analytics/replay-diff join events for the same packet across
/// its lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A packet was generated at its source.
    Injected {
        cycle: u64,
        packet: u32,
        src: u32,
        dst: u32,
        size: u16,
    },
    /// Route compute committed an output port for a head flit.
    Routed {
        cycle: u64,
        router: u32,
        in_port: u8,
        in_vc: u8,
        packet: u32,
        /// Bitmask of admissible ports the routing algorithm returned (bit i
        /// set iff port i was a candidate). Assumes the topology's port count
        /// fits in 16 bits, true of every topology family in scope.
        cand_mask: u16,
        chosen: u8,
    },
    /// VC allocation committed an output VC for a head flit.
    VcAlloc {
        cycle: u64,
        router: u32,
        in_port: u8,
        in_vc: u8,
        packet: u32,
        out_port: u8,
        out_vc: u8,
    },
    /// Switch allocation granted an input VC the crossbar for one cycle.
    SaGrant {
        cycle: u64,
        router: u32,
        in_port: u8,
        in_vc: u8,
        packet: u32,
        out_port: u8,
    },
    /// A flit crossed the crossbar (ejected locally or launched onto a link).
    Traversed {
        cycle: u64,
        router: u32,
        packet: u32,
        out_port: u8,
        out_vc: u8,
        flit: u32,
    },
    /// A packet's tail flit reached its destination.
    Delivered {
        cycle: u64,
        packet: u32,
        src: u32,
        dst: u32,
        latency: u64,
    },
    /// A flit was discarded rather than delivered. Schema-complete for the
    /// columnar contract; not emitted today — the current router model is
    /// lossless (credit-based flow control admits no drop path).
    Dropped {
        cycle: u64,
        router: u32,
        port: u8,
        packet: u32,
    },
    /// Sampled input-buffer occupancy for one router. Produced by
    /// `tessera-record`'s recorder at the metrics interval, not pushed from a
    /// stage kernel.
    BufOccupancy {
        cycle: u64,
        router: u32,
        /// Occupancy in `0..=1000` (milli-fraction), integer so `Eq` holds.
        occ_milli: u16,
    },
    /// A thermal/power throttle turned on for one router. Recorder-sampled,
    /// like `BufOccupancy`.
    ThrottleOn { cycle: u64, router: u32 },
}
