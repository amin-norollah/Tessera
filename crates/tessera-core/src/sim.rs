//! The simulation kernel: a virtual-channel, credit-flow-controlled, wormhole
//! router network driven by an ordered set of per-cycle stage kernels.
//!
//! Per-cycle contract: each stage reads the state at stage entry and applies its
//! effects in a fixed order, so a fixed iteration order (routers x ports x VCs)
//! is deterministic. Link latency (>= 1) guarantees a flit advances at most one
//! hop per cycle. Concrete topology/routing/traffic are injected as trait
//! objects; the router holds no algorithm logic.
//!
//! VC model (Phase 1): each input VC is an independent flit FIFO holding the
//! contiguous flits of at most one in-flight packet at its head. A head flit
//! commits an output *port* (route compute, RC) and then an output *VC* on that
//! port (VC allocation, VA); body/tail flits inherit both. An output VC is held
//! by one input VC from head to tail (the wormhole invariant, per VC). Credits
//! are tracked per output (port, VC). With `num_vcs == 1` this reduces exactly
//! to the Phase 0 single-VC behavior.

use std::collections::VecDeque;

use crate::buffer::BufferPool;
use crate::flit::{FlitArena, FlitHeader, FlitKind};
use crate::ids::{FlitId, PacketId, PortId, RouterId, VcId};
use crate::rng::{NodeRng, RngStreams};
use crate::traits::{
    PacketDesc, RouteCtx, RoutingAlgorithm, SelectionPolicy, Topology, TrafficGenerator,
};

/// The VC a packet is injected on at the local port. Injection uses a single
/// input VC; the network VCs are allocated per packet at VA.
const INJECT_VC: u8 = 0;
const NONE: i16 = -1;
const FREE: i32 = -1;
const INFINITE_CREDIT: u16 = u16::MAX;

/// Structural parameters of a run. All values are runtime, never compile-time.
#[derive(Clone, Copy, Debug)]
pub struct SimConfig {
    pub buffer_depth: u16,
    /// Virtual channels per port.
    pub num_vcs: u8,
    /// Cycles a flit waits in an input buffer before it may be forwarded (t_r).
    pub router_latency: u16,
    /// Cycles a flit spends traversing a link (t_w).
    pub link_latency: u16,
    /// Cycles for a freed buffer slot's credit to return upstream.
    pub credit_delay: u16,
    /// Flits per packet (fixed size in Phase 0/1 synthetic traffic).
    pub packet_size: u16,
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            buffer_depth: 4,
            num_vcs: 1,
            router_latency: 1,
            link_latency: 1,
            credit_delay: 1,
            packet_size: 4,
            seed: 1,
        }
    }
}

/// The lifecycle of an input VC, derived from its buffer + reservation state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VcState {
    /// Empty.
    Idle,
    /// Head present, output port not yet computed.
    Routing,
    /// Output port committed, output VC not yet allocated.
    VcAlloc,
    /// Output port and VC held; the packet is streaming.
    Active,
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
    vcs: u8,
    local_port: u8,
    injecting: bool,
    next_packet_id: u32,

    arena: FlitArena,
    buffers: BufferPool,

    // --- Per input (router, in_port, in_vc) ---
    /// Committed output port for the packet at this input VC's head, or NONE.
    reserve_port: Vec<i16>,
    /// Committed output VC, or NONE.
    reserve_vc: Vec<i16>,

    // --- Per output (router, out_port, out_vc) ---
    /// Wormhole holder (encoded in_port*vcs + in_vc), or FREE.
    hold: Vec<i32>,
    /// Downstream free-slot credits.
    credits: Vec<u16>,

    // --- Per (router, port) ---
    /// In-flight flits per output port: (flit, arrival_cycle). One physical
    /// link per port carries flits of all VCs, one per cycle.
    links: Vec<Vec<(u32, u64)>>,

    /// Scheduled credit returns: (router, port, vc, apply_cycle).
    credit_returns: Vec<(u32, u8, u8, u64)>,

    pe_src: Vec<VecDeque<u32>>,
    pe_rng: Vec<NodeRng>,

    // Per-tick scratch, retained to avoid per-cycle allocation.
    /// SA phase 1: the VC chosen to represent each input port this cycle, or NONE.
    sel_vc: Vec<i16>,
    /// SA phase 2: winning input port per output port, or NONE.
    grant_ip: Vec<i16>,
    /// SA phase 2: winning input VC per output port.
    grant_ivc: Vec<i16>,
    /// Round-robin pointer over VCs, per input port (SA phase 1).
    rr_vc: Vec<u8>,
    /// Round-robin pointer over input ports, per output port (SA phase 2).
    rr_port: Vec<u8>,
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
        let vcs = cfg.num_vcs.max(1);
        let local_port = topo.local_port().0;
        let per_port = n as usize * ports as usize;
        let per_vc = per_port * vcs as usize;

        let streams = RngStreams::new(cfg.seed);
        let pe_rng = (0..n).map(|node| streams.pe(node)).collect();

        let mut credits = vec![0u16; per_vc];
        for r in 0..n {
            for p in 0..ports {
                let base = (r as usize * ports as usize + p as usize) * vcs as usize;
                let value = if p == local_port {
                    INFINITE_CREDIT
                } else if topo.adjacent(RouterId(r), PortId(p)).is_some() {
                    cfg.buffer_depth
                } else {
                    0
                };
                for v in 0..vcs as usize {
                    credits[base + v] = value;
                }
            }
        }

        Self {
            cycle: 0,
            metrics: Metrics::default(),
            measure_start: 0,
            measure_end: 0,
            cfg,
            buffers: BufferPool::new(n, ports, vcs, cfg.buffer_depth),
            ports,
            vcs,
            local_port,
            injecting: true,
            next_packet_id: 0,
            arena: FlitArena::new(),
            reserve_port: vec![NONE; per_vc],
            reserve_vc: vec![NONE; per_vc],
            hold: vec![FREE; per_vc],
            credits,
            links: vec![Vec::new(); per_port],
            credit_returns: Vec::new(),
            pe_src: (0..n).map(|_| VecDeque::new()).collect(),
            pe_rng,
            sel_vc: vec![NONE; per_port],
            grant_ip: vec![NONE; per_port],
            grant_ivc: vec![NONE; per_port],
            rr_vc: vec![0; per_port],
            rr_port: vec![0; per_port],
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

    pub fn num_vcs(&self) -> u8 {
        self.vcs
    }

    pub fn set_injecting(&mut self, on: bool) {
        self.injecting = on;
    }

    /// Flits generated but not yet delivered (includes source-queued flits).
    pub fn in_flight(&self) -> u64 {
        self.metrics.injected_flits - self.metrics.ejected_flits
    }

    /// The lifecycle state of an input VC (for tests/inspection).
    pub fn vc_state(&self, r: RouterId, port: PortId, vc: VcId) -> VcState {
        if self.buffers.len(r.0, port.0, vc.0) == 0 {
            return VcState::Idle;
        }
        let idx = self.civ(r.0, port.0, vc.0);
        if self.reserve_port[idx] == NONE {
            VcState::Routing
        } else if self.reserve_vc[idx] == NONE {
            VcState::VcAlloc
        } else {
            VcState::Active
        }
    }

    /// Index into per-(router, port, vc) arrays.
    #[inline]
    fn civ(&self, r: u32, port: u8, vc: u8) -> usize {
        (r as usize * self.ports as usize + port as usize) * self.vcs as usize + vc as usize
    }

    /// Index into per-(router, port) arrays.
    #[inline]
    fn rp(&self, r: u32, port: u8) -> usize {
        r as usize * self.ports as usize + port as usize
    }

    pub fn tick(&mut self) {
        self.apply_credit_returns();
        self.stage_link_traversal();
        self.stage_inject();
        self.stage_route_compute();
        self.stage_vc_alloc();
        self.stage_switch_alloc();
        self.stage_switch_traversal();
        self.cycle += 1;
    }

    fn apply_credit_returns(&mut self) {
        let cycle = self.cycle;
        let depth = self.cfg.buffer_depth;
        let mut i = 0;
        while i < self.credit_returns.len() {
            let (r, p, v, apply) = self.credit_returns[i];
            if apply == cycle {
                let idx = self.civ(r, p, v);
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
                let link = self.rp(r, p);
                if self.links[link].is_empty() {
                    continue;
                }
                let Some((dr, dp)) = self.topo.adjacent(RouterId(r), PortId(p)) else {
                    continue;
                };
                let mut i = 0;
                while i < self.links[link].len() {
                    let (flit, arrival) = self.links[link][i];
                    if arrival == cycle {
                        let vc = self.arena.get(FlitId(flit)).vc.0;
                        self.arena.get_mut(FlitId(flit)).eligible_cycle = eligible;
                        let pushed = self.buffers.push(dr.0, dp.0, vc, FlitId(flit));
                        debug_assert!(pushed, "credit flow control guarantees a free slot");
                        self.links[link].swap_remove(i);
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
            while !self.pe_src[r as usize].is_empty()
                && self.buffers.free_slots(r, lp, INJECT_VC) > 0
            {
                let flit = self.pe_src[r as usize].pop_front().unwrap();
                self.arena.get_mut(FlitId(flit)).eligible_cycle = eligible;
                self.buffers.push(r, lp, INJECT_VC, FlitId(flit));
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
                vc: VcId(INJECT_VC),
                hops: 0,
                inject_cycle: cycle,
                eligible_cycle: 0,
            };
            let flit = self.arena.alloc(header);
            self.pe_src[r as usize].push_back(flit.0);
        }
    }

    /// RC: a head with no committed output port picks one via the routing
    /// algorithm + selection policy.
    fn stage_route_compute(&mut self) {
        let cycle = self.cycle;
        let n = self.topo.num_routers();
        for r in 0..n {
            for ip in 0..self.ports {
                for ivc in 0..self.vcs {
                    let idx = self.civ(r, ip, ivc);
                    if self.reserve_port[idx] != NONE {
                        continue;
                    }
                    let Some(flit) = self.buffers.front(r, ip, ivc) else {
                        continue;
                    };
                    let header = *self.arena.get(flit);
                    if header.eligible_cycle > cycle || !header.kind.needs_route() {
                        continue;
                    }
                    let ctx = RouteCtx {
                        here: RouterId(r),
                        dst: header.dst,
                        in_port: PortId(ip),
                        topo: &*self.topo,
                    };
                    let cands = self.routing.route(&ctx);
                    self.reserve_port[idx] = self.selection.select(&cands).0 as i16;
                }
            }
        }
    }

    /// VA: a head with a committed port but no output VC acquires a free output
    /// VC on that port. The VC is held from head to tail.
    fn stage_vc_alloc(&mut self) {
        let cycle = self.cycle;
        let n = self.topo.num_routers();
        for r in 0..n {
            for ip in 0..self.ports {
                for ivc in 0..self.vcs {
                    let idx = self.civ(r, ip, ivc);
                    let op = self.reserve_port[idx];
                    if op == NONE || self.reserve_vc[idx] != NONE {
                        continue;
                    }
                    let Some(flit) = self.buffers.front(r, ip, ivc) else {
                        continue;
                    };
                    if self.arena.get(flit).eligible_cycle > cycle {
                        continue;
                    }
                    let op = op as u8;
                    let holder = ip as i32 * self.vcs as i32 + ivc as i32;
                    for ovc in 0..self.vcs {
                        let out = self.civ(r, op, ovc);
                        if self.hold[out] == FREE {
                            self.hold[out] = holder;
                            self.reserve_vc[idx] = ovc as i16;
                            break;
                        }
                    }
                }
            }
        }
    }

    /// SA: two-phase separable allocation. Phase 1 picks one ready VC per input
    /// port (VC arbitration); phase 2 picks one input port per output port (port
    /// arbitration). This enforces at most one flit per input and per output
    /// port per cycle.
    fn stage_switch_alloc(&mut self) {
        let n = self.topo.num_routers();
        let ports = self.ports as usize;
        let vcs = self.vcs as usize;

        for r in 0..n {
            // Phase 1: per input port, round-robin over its VCs for one ready to send.
            for ip in 0..self.ports {
                let slot = self.rp(r, ip);
                self.sel_vc[slot] = NONE;
                let start = self.rr_vc[slot] as usize;
                for k in 0..vcs {
                    let ivc = ((start + k) % vcs) as u8;
                    if self.input_vc_ready(r, ip, ivc) {
                        self.sel_vc[slot] = ivc as i16;
                        self.rr_vc[slot] = ((ivc as usize + 1) % vcs) as u8;
                        break;
                    }
                }
            }
            // Phase 2: per output port, round-robin over input ports requesting it.
            for op in 0..self.ports {
                let out_slot = self.rp(r, op);
                self.grant_ip[out_slot] = NONE;
                self.grant_ivc[out_slot] = NONE;
                let start = self.rr_port[out_slot] as usize;
                for k in 0..ports {
                    let ip = ((start + k) % ports) as u8;
                    let in_slot = self.rp(r, ip);
                    let ivc = self.sel_vc[in_slot];
                    if ivc == NONE {
                        continue;
                    }
                    if self.reserve_port[self.civ(r, ip, ivc as u8)] == op as i16 {
                        self.grant_ip[out_slot] = ip as i16;
                        self.grant_ivc[out_slot] = ivc;
                        self.rr_port[out_slot] = ((ip as usize + 1) % ports) as u8;
                        break;
                    }
                }
            }
        }
    }

    /// A ready-to-send input VC: active (port+VC committed), its head flit is
    /// present and past its router-latency wait, and its output VC has credit.
    fn input_vc_ready(&self, r: u32, ip: u8, ivc: u8) -> bool {
        let idx = self.civ(r, ip, ivc);
        let (op, ovc) = (self.reserve_port[idx], self.reserve_vc[idx]);
        if op == NONE || ovc == NONE {
            return false;
        }
        let Some(flit) = self.buffers.front(r, ip, ivc) else {
            return false;
        };
        if self.arena.get(flit).eligible_cycle > self.cycle {
            return false;
        }
        self.credits[self.civ(r, op as u8, ovc as u8)] > 0
    }

    /// ST: each granted (output port <- input VC) crosses the crossbar. A flit
    /// bound for the local port is ejected; otherwise it is launched onto the
    /// link, a credit is consumed, and the tail releases the reservation.
    fn stage_switch_traversal(&mut self) {
        let cycle = self.cycle;
        let n = self.topo.num_routers();
        let link_latency = self.cfg.link_latency as u64;
        let credit_delay = self.cfg.credit_delay as u64;
        for r in 0..n {
            for op in 0..self.ports {
                let out_slot = self.rp(r, op);
                let ip = self.grant_ip[out_slot];
                if ip == NONE {
                    continue;
                }
                let (ip, ivc) = (ip as u8, self.grant_ivc[out_slot] as u8);
                let Some(flit) = self.buffers.front(r, ip, ivc) else {
                    continue;
                };
                let in_idx = self.civ(r, ip, ivc);
                let ovc = self.reserve_vc[in_idx] as u8;

                if op == self.local_port {
                    self.eject(r, ip, ivc, op, ovc, flit);
                    continue;
                }

                let out_idx = self.civ(r, op, ovc);
                self.buffers.pop(r, ip, ivc);
                self.credits[out_idx] -= 1;
                let header = {
                    let h = self.arena.get_mut(flit);
                    h.hops += 1;
                    h.vc = VcId(ovc);
                    *h
                };
                self.links[out_slot].push((flit.0, cycle + link_latency));
                if header.kind.frees_worm() {
                    self.reserve_port[in_idx] = NONE;
                    self.reserve_vc[in_idx] = NONE;
                    self.hold[out_idx] = FREE;
                }
                self.return_credit(r, ip, ivc, cycle + credit_delay);
            }
        }
    }

    fn eject(&mut self, r: u32, ip: u8, ivc: u8, op: u8, ovc: u8, flit: FlitId) {
        self.buffers.pop(r, ip, ivc);
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
            let in_idx = self.civ(r, ip, ivc);
            let out_idx = self.civ(r, op, ovc);
            self.reserve_port[in_idx] = NONE;
            self.reserve_vc[in_idx] = NONE;
            self.hold[out_idx] = FREE;
        }
        self.arena.free(flit);
        let credit_delay = self.cfg.credit_delay as u64;
        self.return_credit(r, ip, ivc, self.cycle + credit_delay);
    }

    /// Return a credit to the upstream output VC that fed input `(ip, ivc)` of
    /// router `r`. VCs are preserved hop-to-hop, so the upstream VC equals
    /// `ivc`. No-op for the local injection port (fed by the PE).
    fn return_credit(&mut self, r: u32, ip: u8, ivc: u8, apply_cycle: u64) {
        if let Some((ur, up)) = self.topo.adjacent(RouterId(r), PortId(ip)) {
            self.credit_returns.push((ur.0, up.0, ivc, apply_cycle));
        }
    }
}
