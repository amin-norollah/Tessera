//! Selection policies behind the [`SelectionPolicy`] contract: pick one port
//! from the routing algorithm's admissible set (P1-F11).

use tessera_core::{Candidates, CongestionView, NodeRng, PortId, SelectionPolicy};

/// Picks the first admissible port. With deterministic routing there is
/// exactly one candidate; with a turn model it takes the model's
/// highest-priority legal productive port.
pub struct FirstAvailable;

impl SelectionPolicy for FirstAvailable {
    fn select(
        &mut self,
        cands: &Candidates,
        _congestion: &dyn CongestionView,
        _rng: &mut NodeRng,
    ) -> PortId {
        debug_assert!(!cands.is_empty(), "routing must admit at least one port");
        cands.get(0)
    }
}

/// Picks the admissible port with the most free downstream buffer slots
/// (summed across VCs); ties break toward the first candidate. Spreads load
/// away from congested links instead of always preferring the routing
/// algorithm's nominal first choice.
pub struct BufferLevel;

impl SelectionPolicy for BufferLevel {
    fn select(
        &mut self,
        cands: &Candidates,
        congestion: &dyn CongestionView,
        _rng: &mut NodeRng,
    ) -> PortId {
        debug_assert!(!cands.is_empty(), "routing must admit at least one port");
        let mut best = cands.get(0);
        let mut best_slots = congestion.free_slots(best);
        for i in 1..cands.len() {
            let candidate = cands.get(i);
            let slots = congestion.free_slots(candidate);
            if slots > best_slots {
                best = candidate;
                best_slots = slots;
            }
        }
        best
    }
}

/// Rotates across the admissible set on successive calls. The pointer is a
/// plain incrementing index modulo the current candidate count, not a
/// per-physical-port history, so it spreads load over time without tracking
/// per-port state.
#[derive(Default)]
pub struct RoundRobinSelection {
    next: u8,
}

impl RoundRobinSelection {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SelectionPolicy for RoundRobinSelection {
    fn select(
        &mut self,
        cands: &Candidates,
        _congestion: &dyn CongestionView,
        _rng: &mut NodeRng,
    ) -> PortId {
        debug_assert!(!cands.is_empty(), "routing must admit at least one port");
        let i = self.next as usize % cands.len();
        self.next = self.next.wrapping_add(1);
        cands.get(i)
    }
}

/// Picks uniformly at random among the admissible set, drawing from the
/// router's own RNG stream (P1-F18: randomness stays per-node, never a
/// shared/global generator).
pub struct Random;

impl SelectionPolicy for Random {
    fn select(
        &mut self,
        cands: &Candidates,
        _congestion: &dyn CongestionView,
        rng: &mut NodeRng,
    ) -> PortId {
        debug_assert!(!cands.is_empty(), "routing must admit at least one port");
        let i = rng.below(cands.len() as u32) as usize;
        cands.get(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCongestion(Vec<u16>);
    impl CongestionView for FakeCongestion {
        fn free_slots(&self, port: PortId) -> u16 {
            self.0[port.0 as usize]
        }
    }

    fn cands(ports: &[u8]) -> Candidates {
        let mut c = Candidates::new();
        for &p in ports {
            c.push(PortId(p));
        }
        c
    }

    fn rng() -> NodeRng {
        tessera_core::RngStreams::new(1).router(0)
    }

    #[test]
    fn buffer_level_picks_the_most_free_slots() {
        let mut sel = BufferLevel;
        let c = cands(&[0, 1, 2]);
        let congestion = FakeCongestion(vec![1, 5, 3]);
        assert_eq!(sel.select(&c, &congestion, &mut rng()).0, 1);
    }

    #[test]
    fn buffer_level_breaks_ties_toward_first_candidate() {
        let mut sel = BufferLevel;
        let c = cands(&[2, 0]);
        let congestion = FakeCongestion(vec![4, 4, 4]);
        assert_eq!(sel.select(&c, &congestion, &mut rng()).0, 2);
    }

    #[test]
    fn round_robin_selection_cycles_through_candidates() {
        let mut sel = RoundRobinSelection::new();
        let c = cands(&[0, 1, 2]);
        let congestion = FakeCongestion(vec![0, 0, 0]);
        let picks: Vec<u8> = (0..6)
            .map(|_| sel.select(&c, &congestion, &mut rng()).0)
            .collect();
        assert_eq!(picks, vec![0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn random_always_picks_from_the_admissible_set() {
        let mut sel = Random;
        let c = cands(&[1, 3]);
        let congestion = FakeCongestion(vec![0, 0, 0, 0]);
        let mut r = rng();
        for _ in 0..50 {
            let picked = sel.select(&c, &congestion, &mut r).0;
            assert!(picked == 1 || picked == 3, "unexpected pick {picked}");
        }
    }
}
