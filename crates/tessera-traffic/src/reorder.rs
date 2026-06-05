//! Destination reorder buffer (P3.5-F5): non-deterministic (adaptive) routing
//! can deliver packets from the same source out of order relative to how they
//! were injected. Sources that need in-order message semantics (e.g. Netrace
//! dependency release, `crate::netrace`) sequence their outgoing packets per
//! source and reassemble them here before acting on a delivery.

use std::collections::{BTreeMap, HashMap};

use tessera_core::RouterId;

/// A delivery whose sequence number is stale (already released) or a repeat:
/// the source double-delivered `seq`, or delivered it after already having
/// advanced past it. Either indicates a caller bug (duplicate injection, or
/// two calls to [`ReorderBuffer::arrive`] with the same `(src, seq)`), not a
/// condition the network itself can produce.
#[derive(Debug, PartialEq, Eq)]
pub struct StaleArrival {
    pub src: RouterId,
    pub seq: u64,
    pub next_expected: u64,
}

/// Per-source arrival state: the next sequence number this source is
/// expected to deliver, and any later arrivals buffered ahead of it.
#[derive(Debug)]
struct SourceState<T> {
    next: u64,
    pending: BTreeMap<u64, T>,
}

/// Reassembles a per-source sequence of items into delivery order, buffering
/// arrivals that outrun their predecessors. One buffer instance is shared
/// across every source that feeds a given sink; sources are independent of
/// each other (a stall on one source's sequence never blocks another's).
#[derive(Debug, Default)]
pub struct ReorderBuffer<T> {
    sources: HashMap<RouterId, SourceState<T>>,
}

impl<T> ReorderBuffer<T> {
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
        }
    }

    /// Record that `item` arrived from `src` bearing sequence number `seq`
    /// (0-based, assigned by the source in injection order). Returns every
    /// item now releasable in order: `item` itself if it was the next one
    /// expected from `src`, plus any subsequently-buffered items that become
    /// contiguous as a result — otherwise `item` is buffered and an empty
    /// list is returned.
    pub fn arrive(&mut self, src: RouterId, seq: u64, item: T) -> Result<Vec<T>, StaleArrival> {
        let state = self.sources.entry(src).or_insert_with(|| SourceState {
            next: 0,
            pending: BTreeMap::new(),
        });
        if seq < state.next || state.pending.contains_key(&seq) {
            return Err(StaleArrival {
                src,
                seq,
                next_expected: state.next,
            });
        }
        state.pending.insert(seq, item);

        let mut ready = Vec::new();
        while let Some(next_item) = state.pending.remove(&state.next) {
            ready.push(next_item);
            state.next += 1;
        }
        Ok(ready)
    }

    /// Items buffered per source that are still waiting on an earlier
    /// sequence number to arrive (diagnostic/test observable).
    pub fn pending_count(&self, src: RouterId) -> usize {
        self.sources.get(&src).map_or(0, |s| s.pending.len())
    }
}

/// Per source: next-expected sequence number, and any out-of-order items
/// still buffered ahead of it (`(seq, item)` pairs) — the wire shape of
/// [`ReorderBuffer::snapshot`]/[`ReorderBuffer::restore`].
pub type ReorderSnapshot<T> = Vec<(RouterId, u64, Vec<(u64, T)>)>;

impl<T: Clone> ReorderBuffer<T> {
    /// Plain-data view of per-source state for keyframing (P4-F1): for each
    /// source with any recorded activity, its next-expected sequence number
    /// and any out-of-order items still buffered ahead of it.
    pub fn snapshot(&self) -> ReorderSnapshot<T> {
        self.sources
            .iter()
            .map(|(&src, s)| {
                let pending = s
                    .pending
                    .iter()
                    .map(|(&seq, item)| (seq, item.clone()))
                    .collect();
                (src, s.next, pending)
            })
            .collect()
    }

    /// Overwrite this buffer's contents from a prior [`Self::snapshot`].
    pub fn restore(&mut self, snap: ReorderSnapshot<T>) {
        self.sources = snap
            .into_iter()
            .map(|(src, next, pending)| {
                (
                    src,
                    SourceState {
                        next,
                        pending: pending.into_iter().collect(),
                    },
                )
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_arrivals_release_immediately() {
        let mut rob = ReorderBuffer::new();
        for seq in 0..5u64 {
            let released = rob.arrive(RouterId(0), seq, seq).unwrap();
            assert_eq!(released, vec![seq]);
        }
    }

    #[test]
    fn out_of_order_arrivals_release_in_order_once_contiguous() {
        let mut rob = ReorderBuffer::new();
        assert_eq!(rob.arrive(RouterId(0), 2, 2).unwrap(), Vec::<u64>::new());
        assert_eq!(rob.arrive(RouterId(0), 1, 1).unwrap(), Vec::<u64>::new());
        // seq 0 arrives last: releases 0, 1, 2 in order in one call.
        assert_eq!(rob.arrive(RouterId(0), 0, 0).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn sources_are_independent() {
        let mut rob = ReorderBuffer::new();
        assert_eq!(
            rob.arrive(RouterId(0), 1, "a1").unwrap(),
            Vec::<&str>::new()
        );
        // A different source's seq 0 must not be affected by node 0's gap.
        assert_eq!(rob.arrive(RouterId(1), 0, "b0").unwrap(), vec!["b0"]);
        assert_eq!(rob.arrive(RouterId(0), 0, "a0").unwrap(), vec!["a0", "a1"]);
    }

    #[test]
    fn duplicate_or_stale_seq_is_rejected() {
        let mut rob = ReorderBuffer::new();
        assert_eq!(rob.arrive(RouterId(0), 0, 0).unwrap(), vec![0]);
        let err = rob.arrive(RouterId(0), 0, 0).unwrap_err();
        assert_eq!(
            err,
            StaleArrival {
                src: RouterId(0),
                seq: 0,
                next_expected: 1
            }
        );
    }

    #[test]
    fn any_arrival_permutation_releases_the_full_run_exactly_once_in_order() {
        // Property check (INITIAL_PRD reorder/reassembly gate, G3.5.3): for
        // every permutation of a short run, the union of all `arrive` results
        // is the original sequence, in order, with no loss or duplication.
        let n = 6u64;
        let mut perm: Vec<u64> = (0..n).collect();
        let mut permutations_checked = 0;
        // Heap's algorithm over a small fixed-size array keeps this
        // allocation-free and deterministic without a `rand` dependency.
        fn heap_permute(k: usize, arr: &mut Vec<u64>, visit: &mut impl FnMut(&[u64])) {
            if k == 1 {
                visit(arr);
                return;
            }
            for i in 0..k {
                heap_permute(k - 1, arr, visit);
                if k.is_multiple_of(2) {
                    arr.swap(i, k - 1);
                } else {
                    arr.swap(0, k - 1);
                }
            }
        }
        heap_permute(perm.len(), &mut perm, &mut |order| {
            let mut rob: ReorderBuffer<u64> = ReorderBuffer::new();
            let mut released = Vec::new();
            for &seq in order {
                released.extend(rob.arrive(RouterId(0), seq, seq).unwrap());
            }
            assert_eq!(released, (0..n).collect::<Vec<_>>());
            permutations_checked += 1;
        });
        assert_eq!(permutations_checked, 720); // 6! = 720, none skipped
    }
}
