//! Arbiters behind the [`Allocator`] contract, shared by VC allocation (VA)
//! and both switch-allocation phases (SA): one bipartite-matching primitive
//! serves all three (P1-F6). Each keeps its fairness pointers in flat `Vec`s
//! reused across calls (grown once to the largest id seen, then stable), so
//! steady-state arbitration allocates no memory.
//!
//! Requester/resource ids are the caller's flat, cycle-stable indices (e.g.
//! `router*ports*vcs + port*vcs + vc`); an arbiter here only compares and
//! orders them.

use tessera_core::{AllocRequest, Allocator, GrantSet};

const NONE: i64 = -1;
/// Modulus for the wraparound "distance after the pointer" ranking that
/// gives round-robin/iSLIP their fairness: the next id after the pointer
/// (wrapping past `u32::MAX`) ranks first.
const ID_SPACE: i64 = 1i64 << 32;

fn distance_after(id: u32, pointer: i64) -> i64 {
    (id as i64 - pointer - 1).rem_euclid(ID_SPACE)
}

/// Buckets `reqs` by resource into `buckets[..num_resources]`, growing the
/// outer `Vec` (not its already-allocated inner `Vec`s) only the first time a
/// larger resource count is seen.
fn bucket_by_resource(reqs: &[AllocRequest], buckets: &mut Vec<Vec<u32>>, num_resources: usize) {
    if buckets.len() < num_resources {
        buckets.resize_with(num_resources, Vec::new);
    }
    for bucket in &mut buckets[..num_resources] {
        bucket.clear();
    }
    for req in reqs {
        buckets[req.resource as usize].push(req.requester);
    }
}

/// Round-robin arbitration: for each resource, the pending requester nearest
/// after that resource's last winner (wrapping) takes it; the pointer
/// advances only when a grant is made.
#[derive(Default)]
pub struct RoundRobin {
    last_winner: Vec<i64>,
    buckets: Vec<Vec<u32>>,
}

impl RoundRobin {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Allocator for RoundRobin {
    fn allocate(&mut self, reqs: &[AllocRequest], grants: &mut GrantSet) {
        let num_resources = grants.num_resources();
        if self.last_winner.len() < num_resources {
            self.last_winner.resize(num_resources, NONE);
        }
        bucket_by_resource(reqs, &mut self.buckets, num_resources);

        for resource in 0..num_resources {
            let pointer = self.last_winner[resource];
            let winner = self.buckets[resource]
                .iter()
                .copied()
                .filter(|&r| grants.requester_grant(r).is_none())
                .min_by_key(|&r| distance_after(r, pointer));
            if let Some(requester) = winner {
                grants.grant(requester, resource as u32);
                self.last_winner[resource] = requester as i64;
            }
        }
    }

    fn snapshot(&self) -> Option<Vec<u8>> {
        // `buckets` is per-call scratch, rebuilt from `reqs` every `allocate`;
        // only the fairness pointer needs to survive a keyframe.
        Some(bincode::serialize(&self.last_winner).expect("Vec<i64> always serializes"))
    }

    fn restore(&mut self, bytes: &[u8]) {
        self.last_winner = bincode::deserialize(bytes).expect("valid RoundRobin snapshot bytes");
    }
}

/// A matrix-style arbiter: each resource keeps a pairwise priority relation
/// over requesters. The requester that beats every other current bidder for
/// a resource wins it; the winner then loses priority (to every bidder it
/// just beat) for next time, so it cycles to the back of the queue: the
/// same "yield priority to whoever you just beat" rule as the classic NxN
/// switch-allocator matrix arbiter, generalized to the variable-sized
/// requester/resource pools VA also uses. Ties (no established priority
/// either way: e.g. on the first contention between a pair) break toward
/// the lower id, so the result stays deterministic from a cold start.
#[derive(Default)]
pub struct Matrix {
    n: usize,
    /// Flat `n*n`; `higher[i*n+j]` set means requester `i` beats `j`.
    higher: Vec<bool>,
    buckets: Vec<Vec<u32>>,
}

impl Matrix {
    pub fn new() -> Self {
        Self::default()
    }

    fn grow(&mut self, new_n: usize) {
        let mut grown = vec![false; new_n * new_n];
        for i in 0..self.n {
            grown[i * new_n..i * new_n + self.n]
                .copy_from_slice(&self.higher[i * self.n..i * self.n + self.n]);
        }
        self.higher = grown;
        self.n = new_n;
    }

    fn beats(&self, i: u32, j: u32) -> bool {
        self.higher[i as usize * self.n + j as usize]
    }
}

impl Allocator for Matrix {
    fn allocate(&mut self, reqs: &[AllocRequest], grants: &mut GrantSet) {
        let num_resources = grants.num_resources();
        let num_requesters = grants.num_requesters();
        if self.n < num_requesters {
            self.grow(num_requesters);
        }
        bucket_by_resource(reqs, &mut self.buckets, num_resources);

        for resource in 0..num_resources {
            let mut winner: Option<u32> = None;
            for &candidate in &self.buckets[resource] {
                if grants.requester_grant(candidate).is_some() {
                    continue;
                }
                winner = Some(match winner {
                    None => candidate,
                    Some(cur) if self.beats(candidate, cur) => candidate,
                    Some(cur) if self.beats(cur, candidate) => cur,
                    Some(cur) => cur.min(candidate), // no established priority yet
                });
            }
            let Some(winner) = winner else { continue };
            for &candidate in &self.buckets[resource] {
                if candidate != winner && grants.requester_grant(candidate).is_none() {
                    self.higher[winner as usize * self.n + candidate as usize] = false;
                    self.higher[candidate as usize * self.n + winner as usize] = true;
                }
            }
            grants.grant(winner, resource as u32);
        }
    }

    fn snapshot(&self) -> Option<Vec<u8>> {
        // `buckets` is per-call scratch; `(n, higher)` is the priority
        // relation that must survive a keyframe.
        Some(
            bincode::serialize(&(self.n, &self.higher))
                .expect("(usize, Vec<bool>) always serializes"),
        )
    }

    fn restore(&mut self, bytes: &[u8]) {
        let (n, higher): (usize, Vec<bool>) =
            bincode::deserialize(bytes).expect("valid Matrix snapshot bytes");
        self.n = n;
        self.higher = higher;
    }
}

/// iSLIP: multi-iteration request/grant/accept matching. Each iteration,
/// unmatched resources grant to their nearest-after-pointer unmatched bidder
/// (as [`RoundRobin`]); each requester that received a grant accepts its
/// nearest-after-*its own* pointer offer. Grant/accept pointers advance only
/// on the **first** iteration's matches (the standard iSLIP rule: later
/// iterations mop up leftover contention without perturbing next cycle's
/// fairness). `iters` bounds how many rounds run per `allocate` call.
pub struct ISlip {
    iters: u8,
    grant_ptr: Vec<i64>,
    accept_ptr: Vec<i64>,
    candidate: Vec<i64>,
    matched_requester: Vec<bool>,
    matched_resource: Vec<bool>,
    buckets: Vec<Vec<u32>>,
}

impl ISlip {
    pub fn new(iters: u8) -> Self {
        Self {
            iters: iters.max(1),
            grant_ptr: Vec::new(),
            accept_ptr: Vec::new(),
            candidate: Vec::new(),
            matched_requester: Vec::new(),
            matched_resource: Vec::new(),
            buckets: Vec::new(),
        }
    }
}

impl Allocator for ISlip {
    fn allocate(&mut self, reqs: &[AllocRequest], grants: &mut GrantSet) {
        let num_resources = grants.num_resources();
        let num_requesters = grants.num_requesters();
        if self.grant_ptr.len() < num_resources {
            self.grant_ptr.resize(num_resources, NONE);
        }
        if self.accept_ptr.len() < num_requesters {
            self.accept_ptr.resize(num_requesters, NONE);
        }
        if self.candidate.len() < num_requesters {
            self.candidate.resize(num_requesters, NONE);
        }
        bucket_by_resource(reqs, &mut self.buckets, num_resources);

        self.matched_requester.clear();
        self.matched_requester.resize(num_requesters, false);
        self.matched_resource.clear();
        self.matched_resource.resize(num_resources, false);

        for iteration in 0..self.iters {
            for c in &mut self.candidate[..num_requesters] {
                *c = NONE;
            }

            // Grant phase: each unmatched, contested resource offers to the
            // requester nearest after its grant pointer among unmatched bidders.
            for resource in 0..num_resources {
                if self.matched_resource[resource] {
                    continue;
                }
                let pointer = self.grant_ptr[resource];
                let winner = self.buckets[resource]
                    .iter()
                    .copied()
                    .filter(|&r| !self.matched_requester[r as usize])
                    .min_by_key(|&r| distance_after(r, pointer));
                let Some(requester) = winner else { continue };
                let prev = self.candidate[requester as usize];
                let takes_it = prev < 0
                    || distance_after(resource as u32, self.accept_ptr[requester as usize])
                        < distance_after(prev as u32, self.accept_ptr[requester as usize]);
                if takes_it {
                    self.candidate[requester as usize] = resource as i64;
                }
            }

            // Accept phase: each requester with a live candidate this round
            // accepts it; the match is final for this cycle.
            for requester in 0..num_requesters {
                if self.matched_requester[requester] {
                    continue;
                }
                let resource = self.candidate[requester];
                if resource < 0 {
                    continue;
                }
                let resource = resource as usize;
                grants.grant(requester as u32, resource as u32);
                self.matched_requester[requester] = true;
                self.matched_resource[resource] = true;
                if iteration == 0 {
                    self.grant_ptr[resource] = requester as i64;
                    self.accept_ptr[requester] = resource as i64;
                }
            }
        }
    }

    fn snapshot(&self) -> Option<Vec<u8>> {
        // `candidate`/`matched_*`/`buckets` are per-call scratch, reset at the
        // top of every `allocate`; only the two pointer vectors that carry
        // fairness across cycles need to survive a keyframe.
        Some(
            bincode::serialize(&(&self.grant_ptr, &self.accept_ptr))
                .expect("(Vec<i64>, Vec<i64>) always serializes"),
        )
    }

    fn restore(&mut self, bytes: &[u8]) {
        let (grant_ptr, accept_ptr): (Vec<i64>, Vec<i64>) =
            bincode::deserialize(bytes).expect("valid ISlip snapshot bytes");
        self.grant_ptr = grant_ptr;
        self.accept_ptr = accept_ptr;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(requester: u32, resource: u32) -> AllocRequest {
        AllocRequest {
            requester,
            resource,
        }
    }

    #[test]
    fn round_robin_grants_disjoint_requests() {
        let mut alloc = RoundRobin::new();
        let mut grants = GrantSet::new();
        grants.reset(4, 2);
        alloc.allocate(&[req(0, 0), req(1, 1)], &mut grants);
        assert_eq!(grants.resource_winner(0), Some(0));
        assert_eq!(grants.resource_winner(1), Some(1));
    }

    #[test]
    fn round_robin_rotates_priority_on_contention() {
        let mut alloc = RoundRobin::new();
        let mut winners = Vec::new();
        for _ in 0..4 {
            let mut grants = GrantSet::new();
            grants.reset(4, 1);
            alloc.allocate(&[req(0, 0), req(1, 0), req(2, 0), req(3, 0)], &mut grants);
            winners.push(grants.resource_winner(0).unwrap());
        }
        // Every requester gets a turn before anyone repeats.
        let mut sorted = winners.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3], "{winners:?}");
    }

    #[test]
    fn round_robin_never_double_grants_a_requester() {
        // Requester 0 bids for both resources it would accept either of;
        // it must win at most one.
        let mut alloc = RoundRobin::new();
        let mut grants = GrantSet::new();
        grants.reset(2, 2);
        alloc.allocate(&[req(0, 0), req(0, 1), req(1, 1)], &mut grants);
        let won_a = grants.resource_winner(0) == Some(0);
        let won_b = grants.resource_winner(1) == Some(0);
        assert!(
            won_a ^ won_b || (!won_a && !won_b),
            "0 won both: {won_a} {won_b}"
        );
        assert!(
            grants.requester_grant(0).is_some(),
            "0 should win something"
        );
    }

    #[test]
    fn matrix_rotates_the_loser_to_the_front() {
        let mut alloc = Matrix::new();
        let mut first = GrantSet::new();
        first.reset(2, 1);
        alloc.allocate(&[req(0, 0), req(1, 0)], &mut first);
        let round1 = first.resource_winner(0).unwrap();

        let mut second = GrantSet::new();
        second.reset(2, 1);
        alloc.allocate(&[req(0, 0), req(1, 0)], &mut second);
        let round2 = second.resource_winner(0).unwrap();

        assert_ne!(round1, round2, "the round-1 winner must yield next time");
    }

    #[test]
    fn matrix_never_double_grants_a_requester() {
        let mut alloc = Matrix::new();
        let mut grants = GrantSet::new();
        grants.reset(2, 2);
        alloc.allocate(&[req(0, 0), req(0, 1), req(1, 1)], &mut grants);
        assert!(grants.requester_grant(0).is_some());
        let won_both = grants.resource_winner(0) == Some(0) && grants.resource_winner(1) == Some(0);
        assert!(!won_both);
    }

    #[test]
    fn islip_matches_a_disjoint_bipartite_set_in_one_iteration() {
        // 0->0, 1->1 with no contention: a single iteration matches both.
        let mut alloc = ISlip::new(1);
        let mut grants = GrantSet::new();
        grants.reset(2, 2);
        alloc.allocate(&[req(0, 0), req(1, 1)], &mut grants);
        assert_eq!(grants.resource_winner(0), Some(0));
        assert_eq!(grants.resource_winner(1), Some(1));
    }

    #[test]
    fn islip_resolves_contention_across_iterations() {
        // 0 and 1 both want resource 0; 1 also wants resource 1. A single
        // iteration leaves resource 1 unmatched if 1 wins resource 0's grant
        // but resource 1 has no other bidder that round; a second iteration
        // lets the loser of resource 0 pick up resource 1 if eligible. Here
        // only 1 bids for resource 1, so 1 must end up matched somewhere and
        // resource 0's other bidder (0) must get resource 0 once 1 vacates.
        let mut alloc = ISlip::new(4);
        let mut grants = GrantSet::new();
        grants.reset(2, 2);
        alloc.allocate(&[req(0, 0), req(1, 0), req(1, 1)], &mut grants);
        assert_eq!(grants.requester_grant(0), Some(0));
        assert_eq!(grants.requester_grant(1), Some(1));
    }

    #[test]
    fn islip_pointer_advances_only_on_first_iteration_accept() {
        let mut alloc = ISlip::new(1);
        let mut first = GrantSet::new();
        first.reset(2, 1);
        alloc.allocate(&[req(0, 0), req(1, 0)], &mut first);
        let round1 = first.resource_winner(0).unwrap();

        let mut second = GrantSet::new();
        second.reset(2, 1);
        alloc.allocate(&[req(0, 0), req(1, 0)], &mut second);
        let round2 = second.resource_winner(0).unwrap();

        assert_ne!(
            round1, round2,
            "grant pointer should rotate after an accept"
        );
    }
}
