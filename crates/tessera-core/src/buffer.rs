//! Structure-of-arrays input-buffer storage.
//!
//! One flat ring-buffer pool for the whole network, logically indexed
//! `[router][port][vc][slot]` and flattened to 1-D. Depth is a runtime value,
//! not a compile-time constant, and every operation is O(1): replacing
//! PAT-Noxim's per-object deques and linear scans.

use crate::ids::FlitId;

const EMPTY: u32 = u32::MAX;

pub struct BufferPool {
    depth: u16,
    ports: u8,
    vcs: u8,
    n_routers: u32,
    slots: Vec<u32>,
    head: Vec<u16>,
    len: Vec<u16>,
}

impl BufferPool {
    pub fn new(n_routers: u32, ports: u8, vcs: u8, depth: u16) -> Self {
        assert!(depth >= 1, "buffer depth must be >= 1");
        let queues = n_routers as usize * ports as usize * vcs as usize;
        Self {
            depth,
            ports,
            vcs,
            n_routers,
            slots: vec![EMPTY; queues * depth as usize],
            head: vec![0; queues],
            len: vec![0; queues],
        }
    }

    #[inline]
    fn queue(&self, r: u32, p: u8, v: u8) -> usize {
        debug_assert!(r < self.n_routers && p < self.ports && v < self.vcs);
        (r as usize * self.ports as usize + p as usize) * self.vcs as usize + v as usize
    }

    #[inline]
    fn slot(&self, queue: usize, offset: u16) -> usize {
        queue * self.depth as usize + offset as usize
    }

    #[inline]
    pub fn depth(&self) -> u16 {
        self.depth
    }

    #[inline]
    pub fn free_slots(&self, r: u32, p: u8, v: u8) -> u16 {
        let q = self.queue(r, p, v);
        self.depth - self.len[q]
    }

    #[inline]
    pub fn len(&self, r: u32, p: u8, v: u8) -> u16 {
        self.len[self.queue(r, p, v)]
    }

    /// Push a flit to the tail. Returns `false` (no-op) if the queue is full.
    pub fn push(&mut self, r: u32, p: u8, v: u8, f: FlitId) -> bool {
        let q = self.queue(r, p, v);
        if self.len[q] == self.depth {
            return false;
        }
        let offset = (self.head[q] + self.len[q]) % self.depth;
        let s = self.slot(q, offset);
        self.slots[s] = f.0;
        self.len[q] += 1;
        true
    }

    /// The flit at the head, without removing it.
    #[inline]
    pub fn front(&self, r: u32, p: u8, v: u8) -> Option<FlitId> {
        let q = self.queue(r, p, v);
        if self.len[q] == 0 {
            return None;
        }
        Some(FlitId(self.slots[self.slot(q, self.head[q])]))
    }

    /// Remove and return the flit at the head.
    pub fn pop(&mut self, r: u32, p: u8, v: u8) -> Option<FlitId> {
        let q = self.queue(r, p, v);
        if self.len[q] == 0 {
            return None;
        }
        let f = FlitId(self.slots[self.slot(q, self.head[q])]);
        self.head[q] = (self.head[q] + 1) % self.depth;
        self.len[q] -= 1;
        Some(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_is_fifo_and_bounded() {
        let mut pool = BufferPool::new(2, 5, 1, 3);
        assert_eq!(pool.free_slots(0, 0, 0), 3);
        assert!(pool.push(0, 0, 0, FlitId(10)));
        assert!(pool.push(0, 0, 0, FlitId(11)));
        assert!(pool.push(0, 0, 0, FlitId(12)));
        assert!(!pool.push(0, 0, 0, FlitId(13)), "full queue rejects push");
        assert_eq!(pool.free_slots(0, 0, 0), 0);

        assert_eq!(pool.front(0, 0, 0), Some(FlitId(10)));
        assert_eq!(pool.pop(0, 0, 0), Some(FlitId(10)));
        assert_eq!(pool.pop(0, 0, 0), Some(FlitId(11)));
        // wrap-around
        assert!(pool.push(0, 0, 0, FlitId(20)));
        assert_eq!(pool.pop(0, 0, 0), Some(FlitId(12)));
        assert_eq!(pool.pop(0, 0, 0), Some(FlitId(20)));
        assert_eq!(pool.pop(0, 0, 0), None);
    }

    #[test]
    fn queues_are_independent() {
        let mut pool = BufferPool::new(2, 5, 1, 2);
        assert!(pool.push(0, 1, 0, FlitId(1)));
        assert!(pool.push(1, 1, 0, FlitId(2)));
        assert_eq!(pool.front(0, 1, 0), Some(FlitId(1)));
        assert_eq!(pool.front(1, 1, 0), Some(FlitId(2)));
        assert_eq!(pool.len(0, 0, 0), 0);
    }
}
