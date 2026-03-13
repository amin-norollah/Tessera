//! The simulation kernel: a single-VC, credit-flow-controlled, wormhole router
//! network driven by an ordered set of per-cycle stage kernels.
//!
//! Per-cycle contract (Phase 0, single VC): each stage reads the state at stage
//! entry and applies its effects in a fixed order, so a fixed iteration order
//! is deterministic. Link latency (>= 1) guarantees a flit advances at most one
//! hop per cycle. Concrete topology/routing/traffic are injected as trait
//! objects — the router holds no algorithm logic.

use std::collections::VecDeque;

use crate::buffer::BufferPool;
use crate::flit::{FlitArena, FlitHeader, FlitKind};
use crate::ids::{FlitId, PacketId, PortId, RouterId, VcId};
use crate::rng::{NodeRng, RngStreams};
use crate::traits::{
    PacketDesc, RouteCtx, RoutingAlgorithm, SelectionPolicy, Topology, TrafficGenerator,
};

const VC0: u8 = 0;
const NONE: i16 = -1;
const INFINITE_CREDIT: u16 = u16::MAX;

/// Structural parameters of a run. All values are runtime, never compile-time.
#[derive(Clone, Copy, Debug)]
pub struct SimConfig {
    pub buffer_depth: u16,
    /// Cycles a flit waits in an input buffer before it may be forwarded (t_r).
    pub router_latency: u16,
    /// Cycles a flit spends traversing a link (t_w).
    pub link_latency: u16,
    /// Cycles for a freed buffer slot's credit to return upstream.
    pub credit_delay: u16,
    /// Flits per packet (fixed in Phase 0).
    pub packet_size: u16,
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            buffer_depth: 4,
            router_latency: 1,
            link_latency: 1,
            credit_delay: 1,
            packet_size: 4,
            seed: 1,
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Metrics {
    pub injected_packets: u64,
    pub injected_flits: u64,
    pub ejected_packets: u64,
    pub ejected_flits: u64,
    /// Packets injected inside the measurement window and fully delivered.
    pub measured_packets: u64,
    /// Sum of end-to-end latencies of the measured packets.
    pub latency_sum: u128,
    /// Flits ejected while the clock was inside the measurement window.
    pub ejected_flits_in_window: u64,
}

impl Metrics {
    pub fn avg_packet_latency(&self) -> f64 {
        if self.measured_packets == 0 {
            0.0
        } else {
            self.latency_sum as f64 / self.measured_packets as f64
        }
    }
}

pub struct Simulation {
    pub cycle: u64,
    pub metrics: Metrics,
    pub measure_start: u64,
    pub measure_end: u64,

    cfg: SimConfig,
    topo: Box<dyn Topology>,
    routing: Box<dyn RoutingAlgorithm>,
    selection: Box<dyn SelectionPolicy>,
    traffic: Box<dyn TrafficGenerator>,

    ports: u8,
    local_port: u8,
    injecting: bool,
    next_packet_id: u32,

    arena: FlitArena,
    buffers: BufferPool,

    /// Downstream free-slot credits, per (router, out_port).
    credits: Vec<u16>,
    /// Output-port wormhole reservation: input port holding it, or NONE.
    hold: Vec<i16>,
    /// Input-port route reservation: committed output port, or NONE.
    reserve: Vec<i16>,
    /// In-flight flits per (router, out_port): (flit, arrival_cycle).
    links: Vec<Vec<(u32, u64)>>,
    /// Scheduled credit returns: (router, out_port, apply_cycle).
    credit_returns: Vec<(u32, u8, u64)>,

    pe_src: Vec<VecDeque<u32>>,
    pe_rng: Vec<NodeRng>,

    // Per-tick scratch, retained to avoid per-cycle allocation.
    req_out: Vec<i16>,
    grants: Vec<i16>,
    rr: Vec<u8>,
    gen_scratch: Vec<PacketDesc>,
}

impl Simulation {
    pub fn new(
        cfg: SimConfig,
        topo: Box<dyn Topology>,
        routing: Box<dyn RoutingAlgorithm>,
        selection: Box<dyn SelectionPolicy>,
        traffic: Box<dyn TrafficGenerator>,
    ) -> Self {
        let n = topo.num_routers();
        let ports = topo.port_count();
        let local_port = topo.local_port().0;
        let cells = n as usize * ports as usize;

        let streams = RngStreams::new(cfg.seed);
        let pe_rng = (0..n).map(|node| streams.pe(node)).collect();

        let mut credits = vec![0u16; cells];
        for r in 0..n {
            for p in 0..ports {
                let idx = r as usize * ports as usize + p as usize;
                credits[idx] = if p == local_port {
                    INFINITE_CREDIT
                } else if topo.adjacent(RouterId(r), PortId(p)).is_some() {
                    cfg.buffer_depth
                } else {
                    0
                };
            }
        }

        Self {
            cycle: 0,
            metrics: Metrics::default(),
            measure_start: 0,
            measure_end: 0,
            cfg,
            buffers: BufferPool::new(n, ports, 1, cfg.buffer_depth),
            ports,
            local_port,
            injecting: true,
            next_packet_id: 0,
            arena: FlitArena::new(),
            credits,
            hold: vec![NONE; cells],
            reserve: vec![NONE; cells],
            links: vec![Vec::new(); cells],
            credit_returns: Vec::new(),
            pe_src: (0..n).map(|_| VecDeque::new()).collect(),
            pe_rng,
            req_out: vec![NONE; cells],
            grants: vec![NONE; cells],
            rr: vec![0; cells],
            gen_scratch: Vec::new(),
            topo,
            routing,
            selection,
            traffic,
        }
    }

    pub fn num_nodes(&self) -> u32 {
        self.topo.num_routers()
    }

    pub fn set_injecting(&mut self, on: bool) {
        self.injecting = on;
    }

    /// Flits generated but not yet delivered (includes source-queued flits).
    pub fn in_flight(&self) -> u64 {
        self.metrics.injected_flits - self.metrics.ejected_flits
    }

    #[inline]
    fn idx(&self, r: u32, p: u8) -> usize {
        r as usize * self.ports as usize + p as usize
    }

    pub fn tick(&mut self) {
        self.apply_credit_returns();
        self.stage_link_traversal();
        self.stage_inject();
        self.stage_route_compute();
        self.stage_switch_alloc();
        self.stage_switch_traversal();
        self.cycle += 1;
    }

    fn apply_credit_returns(&mut self) {
        let cycle = self.cycle;
        let depth = self.cfg.buffer_depth;
        let mut i = 0;
        while i < self.credit_returns.len() {
            let (r, p, apply) = self.credit_returns[i];
            if apply == cycle {
                let idx = self.idx(r, p);
                if self.credits[idx] < depth {
                    self.credits[idx] += 1;
                }
                self.credit_returns.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    fn stage_link_traversal(&mut self) {
        let cycle = self.cycle;
        let eligible = cycle + self.cfg.router_latency as u64;
        let n = self.topo.num_routers();
        for r in 0..n {
            for p in 0..self.ports {
                let idx = self.idx(r, p);
                if self.links[idx].is_empty() {
                    continue;
                }
                let Some((dr, dp)) = self.topo.adjacent(RouterId(r), PortId(p)) else {
                    continue;
                };
                let mut i = 0;
                while i < self.links[idx].len() {
                    let (flit, arrival) = self.links[idx][i];
                    if arrival == cycle {
                        self.arena.get_mut(FlitId(flit)).eligible_cycle = eligible;
                        let pushed = self.buffers.push(dr.0, dp.0, VC0, FlitId(flit));
                        debug_assert!(pushed, "credit flow control guarantees a free slot");
                        self.links[idx].swap_remove(i);
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }

    fn stage_inject(&mut self) {
        let cycle = self.cycle;
        let n = self.topo.num_routers();
        if self.injecting {
            for r in 0..n {
                self.gen_scratch.clear();
                self.traffic.on_tick(
                    cycle,
                    RouterId(r),
                    &mut self.pe_rng[r as usize],
                    &mut self.gen_scratch,
                );
                for k in 0..self.gen_scratch.len() {
                    let desc = self.gen_scratch[k];
                    self.packetize(r, desc, cycle);
                }
            }
        }

        let lp = self.local_port;
        let eligible = cycle + self.cfg.router_latency as u64;
        for r in 0..n {
            while !self.pe_src[r as usize].is_empty() && self.buffers.free_slots(r, lp, VC0) > 0 {
                let flit = self.pe_src[r as usize].pop_front().unwrap();
                self.arena.get_mut(FlitId(flit)).eligible_cycle = eligible;
                self.buffers.push(r, lp, VC0, FlitId(flit));
            }
        }
    }

    fn packetize(&mut self, r: u32, desc: PacketDesc, cycle: u64) {
        let pid = PacketId(self.next_packet_id);
        self.next_packet_id += 1;
        let size = desc.size.max(1);
        self.metrics.injected_packets += 1;
        self.metrics.injected_flits += size as u64;
        for seq in 0..size {
            let kind = match (size, seq) {
                (1, _) => FlitKind::HeadTail,
                (_, 0) => FlitKind::Head,
                (_, s) if s == size - 1 => FlitKind::Tail,
                _ => FlitKind::Body,
            };
            let header = FlitHeader {
                packet: pid,
                kind,
                src: RouterId(r),
                dst: desc.dst,
                seq,
                vc: VcId(VC0),
                hops: 0,
                inject_cycle: cycle,
                eligible_cycle: 0,
            };
            let flit = self.arena.alloc(header);
            self.pe_src[r as usize].push_back(flit.0);
        }
    }

    fn stage_route_compute(&mut self) {
        let cycle = self.cycle;
        let n = self.topo.num_routers();
        for r in 0..n {
            for ip in 0..self.ports {
                let idx = self.idx(r, ip);
                self.req_out[idx] = NONE;
                let Some(flit) = self.buffers.front(r, ip, VC0) else {
                    continue;
                };
                let header = *self.arena.get(flit);
                if header.eligible_cycle > cycle {
                    continue;
                }
                let reserved = self.reserve[idx];
                if reserved != NONE {
                    self.req_out[idx] = reserved;
                } else if header.kind.needs_route() {
                    let ctx = RouteCtx {
                        here: RouterId(r),
                        dst: header.dst,
                        in_port: PortId(ip),
                        topo: &*self.topo,
                    };
                    let cands = self.routing.route(&ctx);
                    self.req_out[idx] = self.selection.select(&cands).0 as i16;
                }
            }
        }
    }

    fn stage_switch_alloc(&mut self) {
        let n = self.topo.num_routers();
        let ports = self.ports as usize;
        for r in 0..n {
            for op in 0..self.ports {
                let out_idx = self.idx(r, op);
                self.grants[out_idx] = NONE;
                let held = self.hold[out_idx];
                if held != NONE {
                    // Wormhole continuation: only the holder may use this port,
                    // and only when its next flit is present and eligible.
                    if self.req_out[self.idx(r, held as u8)] == op as i16 {
                        self.grants[out_idx] = held;
                    }
                    continue;
                }
                // Free port: round-robin among heads requesting it.
                let start = self.rr[out_idx] as usize;
                for k in 0..ports {
                    let ip = ((start + k) % ports) as u8;
                    let in_idx = self.idx(r, ip);
                    if self.req_out[in_idx] == op as i16 && self.reserve[in_idx] == NONE {
                        self.hold[out_idx] = ip as i16;
                        self.reserve[in_idx] = op as i16;
                        self.rr[out_idx] = ((ip as usize + 1) % ports) as u8;
                        self.grants[out_idx] = ip as i16;
                        break;
                    }
                }
            }
        }
    }

    fn stage_switch_traversal(&mut self) {
        let cycle = self.cycle;
        let n = self.topo.num_routers();
        let link_latency = self.cfg.link_latency as u64;
        let credit_delay = self.cfg.credit_delay as u64;
        for r in 0..n {
            for op in 0..self.ports {
                let out_idx = self.idx(r, op);
                let winner = self.grants[out_idx];
                if winner == NONE {
                    continue;
                }
                let ip = winner as u8;
                let Some(flit) = self.buffers.front(r, ip, VC0) else {
                    continue;
                };

                if op == self.local_port {
                    self.eject(r, ip, op, flit);
                    continue;
                }

                if self.credits[out_idx] == 0 {
                    continue; // no downstream slot; worm stalls, reservation held
                }
                self.buffers.pop(r, ip, VC0);
                self.credits[out_idx] -= 1;
                let header = {
                    let h = self.arena.get_mut(flit);
                    h.hops += 1;
                    *h
                };
                self.links[out_idx].push((flit.0, cycle + link_latency));
                if header.kind.frees_worm() {
                    let in_idx = self.idx(r, ip);
                    self.reserve[in_idx] = NONE;
                    self.hold[out_idx] = NONE;
                }
                self.return_credit(r, ip, cycle + credit_delay);
            }
        }
    }

    fn eject(&mut self, r: u32, ip: u8, op: u8, flit: FlitId) {
        self.buffers.pop(r, ip, VC0);
        let header = *self.arena.get(flit);
        self.metrics.ejected_flits += 1;
        if self.cycle >= self.measure_start && self.cycle < self.measure_end {
            self.metrics.ejected_flits_in_window += 1;
        }
        if header.kind.frees_worm() {
            self.metrics.ejected_packets += 1;
            if header.inject_cycle >= self.measure_start && header.inject_cycle < self.measure_end {
                self.metrics.measured_packets += 1;
                self.metrics.latency_sum += (self.cycle - header.inject_cycle) as u128;
            }
            let in_idx = self.idx(r, ip);
            let out_idx = self.idx(r, op);
            self.reserve[in_idx] = NONE;
            self.hold[out_idx] = NONE;
        }
        self.arena.free(flit);
        let credit_delay = self.cfg.credit_delay as u64;
        self.return_credit(r, ip, self.cycle + credit_delay);
    }

    /// Return a credit to whoever fed input `ip` of router `r` (no-op for the
    /// local injection port, which is fed by the PE).
    fn return_credit(&mut self, r: u32, ip: u8, apply_cycle: u64) {
        if let Some((ur, up)) = self.topo.adjacent(RouterId(r), PortId(ip)) {
            self.credit_returns.push((ur.0, up.0, apply_cycle));
        }
    }
}
