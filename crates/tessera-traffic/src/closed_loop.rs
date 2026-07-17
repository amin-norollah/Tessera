//! Closed-loop / producer-gated injection (P3.5-F4): wraps any
//! [`TrafficGenerator`] to cap the number of outstanding (injected but not
//! yet delivered) packets per node, the standard "finite outstanding
//! requests" throttle for closed-loop synthetic traffic (RESEARCH_NOTES C8).
//!
//! Composes like [`crate::Bimodal`]: it changes *when* the wrapped generator's
//! packets are allowed to leave, never their destination/size, so it stacks
//! with `Bimodal` and with dependency-gated sources like
//! [`crate::netrace::Netrace`] (whose own gating this simply adds a second,
//! independent cap on top of).

use std::sync::atomic::{AtomicU32, Ordering};

use tessera_core::{Delivery, NodeRng, PacketDesc, RouterId, TrafficGenerator};

/// Caps outstanding packets per node at `limit`: once a node has `limit`
/// packets in flight, `on_tick` drops any further packets the wrapped
/// generator would have injected that cycle (rather than queuing them — a
/// dropped packet's generator gets another chance to inject next cycle, same
/// as a real closed-loop PE that simply doesn't issue a new request while
/// its MSHRs are full).
pub struct ClosedLoop<G> {
    inner: G,
    limit: u32,
    /// Outstanding count per node. `Relaxed` is sufficient: the kernel drives
    /// `on_tick`/`on_deliver` from a single thread today (OUT-12), and a
    /// future parallel kernel would still need per-node ordering no stronger
    /// than this counter provides, since each node's own increments/
    /// decrements are already serialized by the kernel's fixed stage order.
    outstanding: Vec<AtomicU32>,
}

impl<G> ClosedLoop<G> {
    pub fn new(inner: G, num_nodes: u32, limit: u32) -> Self {
        assert!(
            limit >= 1,
            "closed-loop limit must allow at least 1 outstanding packet"
        );
        Self {
            inner,
            limit,
            outstanding: (0..num_nodes).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    /// Outstanding packets currently charged to `node` (test/diagnostic
    /// observable).
    pub fn outstanding(&self, node: RouterId) -> u32 {
        self.outstanding[node.index()].load(Ordering::Relaxed)
    }
}

impl<G: TrafficGenerator> TrafficGenerator for ClosedLoop<G> {
    fn on_tick(&self, cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        let start = out.len();
        self.inner.on_tick(cycle, node, rng, out);
        let counter = &self.outstanding[node.index()];
        let mut i = start;
        while i < out.len() {
            if counter
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                    (c < self.limit).then_some(c + 1)
                })
                .is_err()
            {
                out.truncate(i);
                break;
            }
            i += 1;
        }
    }

    fn on_deliver(&self, cycle: u64, delivery: Delivery) {
        let counter = &self.outstanding[delivery.src.index()];
        let prev = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| c.checked_sub(1));
        debug_assert!(
            prev.is_ok(),
            "delivery for node {:?} with no recorded outstanding packet",
            delivery.src
        );
        self.inner.on_deliver(cycle, delivery);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::RngStreams;

    /// Always injects one packet per call, tag `1`, so tests can track it
    /// through delivery without depending on a real kernel run.
    struct AlwaysInject {
        dst: RouterId,
    }

    impl TrafficGenerator for AlwaysInject {
        fn on_tick(
            &self,
            _cycle: u64,
            _node: RouterId,
            _rng: &mut NodeRng,
            out: &mut Vec<PacketDesc>,
        ) {
            out.push(PacketDesc {
                dst: self.dst,
                size: 4,
                tag: 1,
            });
        }
    }

    #[test]
    fn caps_outstanding_packets_per_node() {
        let gen = ClosedLoop::new(AlwaysInject { dst: RouterId(1) }, 4, 2);
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();

        for cycle in 0..5 {
            out.clear();
            gen.on_tick(cycle, RouterId(0), &mut rng, &mut out);
            if cycle < 2 {
                assert_eq!(out.len(), 1, "cycle {cycle} should still have room");
            } else {
                assert!(
                    out.is_empty(),
                    "cycle {cycle} should be throttled at the cap"
                );
            }
        }
        assert_eq!(gen.outstanding(RouterId(0)), 2);
    }

    #[test]
    fn delivery_frees_a_slot_for_the_injecting_node() {
        let gen = ClosedLoop::new(AlwaysInject { dst: RouterId(1) }, 4, 1);
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();

        out.clear();
        gen.on_tick(0, RouterId(0), &mut rng, &mut out);
        assert_eq!(out.len(), 1);
        out.clear();
        gen.on_tick(1, RouterId(0), &mut rng, &mut out);
        assert!(out.is_empty(), "at cap, no room for a second packet");

        gen.on_deliver(
            2,
            Delivery {
                src: RouterId(0),
                dst: RouterId(1),
                size: 4,
                tag: 1,
                inject_cycle: 0,
            },
        );
        out.clear();
        gen.on_tick(3, RouterId(0), &mut rng, &mut out);
        assert_eq!(out.len(), 1, "delivery should have freed the slot");
    }

    #[test]
    fn nodes_are_throttled_independently() {
        let gen = ClosedLoop::new(AlwaysInject { dst: RouterId(2) }, 4, 1);
        let mut rng0 = RngStreams::new(1).pe(0);
        let mut rng1 = RngStreams::new(1).pe(1);
        let mut out = Vec::new();

        out.clear();
        gen.on_tick(0, RouterId(0), &mut rng0, &mut out);
        assert_eq!(out.len(), 1);
        out.clear();
        gen.on_tick(0, RouterId(1), &mut rng1, &mut out);
        assert_eq!(out.len(), 1, "node 1's cap is independent of node 0's");
    }
}
