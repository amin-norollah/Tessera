//! Netrace import (P3.5-F1/F2): replay a dependency-annotated message trace,
//! releasing each message only once every dependency predecessor has been
//! delivered (RESEARCH_NOTES E3). Preserves the workload's critical path
//! without running a full CPU/cache model.
//!
//! # Format
//!
//! This reads **Tessera's own trace interchange format** ("TNI v1"), not the
//! upstream `libnetrace` binary `.tra`/library format the Netrace paper ships:
//! decoding that format needs Netrace's C library, which isn't vendored here
//! (see `docs/OUTSTANDING_PRD.md` OUT-24, the same seam pattern as the
//! DSENT/ORION/HotSpot/McPAT FFI crates). TNI is a plain, documented,
//! line-oriented format so traces can be hand-written for tests or generated
//! from PARSEC-derived data by an external conversion step:
//!
//! ```text
//! # comment lines and blank lines are ignored
//! version 1
//! # id  src  dst  class    deps (comma-separated message ids, or `-`)
//! 0      0    3    control  -
//! 1      0    3    data     0
//! 2      3    5    control  1
//! ```
//!
//! `class` is `control` or `data` (bimodal sizing consistent with coherence
//! traffic, P3.5 scope); actual flit counts for each class are supplied by
//! the caller via [`MessageSizing`], not the trace file, so the same trace
//! replays at different granularities.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::BufRead;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tessera_core::{Delivery, NodeRng, PacketDesc, RouterId, TrafficGenerator};

use crate::reorder::ReorderBuffer;

/// Bimodal message-size class consistent with coherence traffic (control vs
/// cache-line, P3.5 scope); shared by [`crate::netrace`] traces and
/// [`crate::synfull`] models so both readers describe sizing the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeClass {
    Control,
    Data,
}

/// Flit counts for each [`SizeClass`] (bimodal: short control vs long data).
#[derive(Clone, Copy, Debug)]
pub struct MessageSizing {
    pub control_flits: u16,
    pub data_flits: u16,
}

impl MessageSizing {
    pub fn flits(&self, class: SizeClass) -> u16 {
        match class {
            SizeClass::Control => self.control_flits,
            SizeClass::Data => self.data_flits,
        }
    }
}

/// One parsed trace record, in trace-native (pre node-mapping) node ids.
#[derive(Clone, Debug)]
pub struct NetraceMessage {
    pub id: u64,
    pub src: u32,
    pub dst: u32,
    pub size_class: SizeClass,
    pub deps: Vec<u64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NetraceError {
    #[error("line {line}: {reason}")]
    Malformed { line: usize, reason: String },
    #[error("unsupported trace format version {found} (this build supports 1)")]
    UnsupportedVersion { found: String },
    #[error("message {id} depends on unknown message {dep}")]
    UnknownDependency { id: u64, dep: u64 },
    #[error("duplicate message id {id} (line {line})")]
    DuplicateId { id: u64, line: usize },
    #[error("dependency cycle detected (first unresolved message: {id})")]
    DependencyCycle { id: u64 },
    #[error("node {id} out of range for a {mesh_nodes}-node mesh")]
    NodeOutOfRange { id: u32, mesh_nodes: u32 },
}

/// A parsed, DAG-validated Netrace-format trace: [`NetraceTrace::from_reader`]
/// rejects unknown dependencies and dependency cycles up front (the Phase 3.5
/// PRD's stated risk mitigation), so [`Netrace`] can assume a valid DAG.
#[derive(Clone, Debug)]
pub struct NetraceTrace {
    pub messages: Vec<NetraceMessage>,
}

impl NetraceTrace {
    pub fn parse(text: &str) -> Result<Self, NetraceError> {
        Self::from_reader(text.as_bytes())
    }

    /// Parses the whole trace into memory. Adequate for the traces this crate
    /// ships and tests against; bounded-memory streaming for multi-gigabyte
    /// traces is tracked as OUT-25 (P3.5-N2) rather than attempted here, since
    /// a correct streaming reader needs a bounded-lookahead dependency
    /// resolution strategy that deserves its own design.
    pub fn from_reader(r: impl BufRead) -> Result<Self, NetraceError> {
        let mut messages = Vec::new();
        let mut seen_version = false;
        let mut ids = HashMap::new();

        for (line_no, line) in r.lines().enumerate() {
            let line_no = line_no + 1;
            let line = line.map_err(|e| NetraceError::Malformed {
                line: line_no,
                reason: e.to_string(),
            })?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if !seen_version {
                let found = line.strip_prefix("version").map(str::trim);
                match found {
                    Some("1") => {
                        seen_version = true;
                        continue;
                    }
                    Some(other) => {
                        return Err(NetraceError::UnsupportedVersion {
                            found: other.to_string(),
                        })
                    }
                    None => {
                        return Err(NetraceError::Malformed {
                            line: line_no,
                            reason: "expected a `version 1` line before any messages".to_string(),
                        })
                    }
                }
            }

            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() != 5 {
                return Err(NetraceError::Malformed {
                    line: line_no,
                    reason: format!("expected 5 fields, found {}", fields.len()),
                });
            }
            let (id, src, dst, class, deps) =
                (fields[0], fields[1], fields[2], fields[3], fields[4]);
            let parse_u64 = |s: &str, what: &str| {
                s.parse::<u64>().map_err(|_| NetraceError::Malformed {
                    line: line_no,
                    reason: format!("{what} '{s}' is not a non-negative integer"),
                })
            };
            let id = parse_u64(id, "id")?;
            let src: u32 =
                parse_u64(src, "src")?
                    .try_into()
                    .map_err(|_| NetraceError::Malformed {
                        line: line_no,
                        reason: "src does not fit a u32".to_string(),
                    })?;
            let dst: u32 =
                parse_u64(dst, "dst")?
                    .try_into()
                    .map_err(|_| NetraceError::Malformed {
                        line: line_no,
                        reason: "dst does not fit a u32".to_string(),
                    })?;
            let size_class = match class {
                "control" => SizeClass::Control,
                "data" => SizeClass::Data,
                other => {
                    return Err(NetraceError::Malformed {
                        line: line_no,
                        reason: format!("size class must be 'control' or 'data', found '{other}'"),
                    })
                }
            };
            let deps = if deps == "-" {
                Vec::new()
            } else {
                deps.split(',')
                    .map(|d| parse_u64(d, "dependency"))
                    .collect::<Result<Vec<_>, _>>()?
            };

            if ids.insert(id, messages.len()).is_some() {
                return Err(NetraceError::DuplicateId { id, line: line_no });
            }
            messages.push(NetraceMessage {
                id,
                src,
                dst,
                size_class,
                deps,
            });
        }
        if !seen_version {
            return Err(NetraceError::Malformed {
                line: 0,
                reason: "empty trace: expected a `version 1` line".to_string(),
            });
        }

        for m in &messages {
            for dep in &m.deps {
                if !ids.contains_key(dep) {
                    return Err(NetraceError::UnknownDependency {
                        id: m.id,
                        dep: *dep,
                    });
                }
            }
        }
        validate_acyclic(&messages, &ids)?;

        Ok(Self { messages })
    }

    /// Keeps only messages with `start <= id < end`; a dependency pointing
    /// outside the kept range is dropped (treated as already satisfied
    /// before the region starts), the same "warm-up skip" assumption
    /// Netrace's own tooling makes (P3.5-F1 region selection).
    pub fn select_region(&self, start: u64, end: u64) -> Self {
        let kept: HashSet<u64> = self
            .messages
            .iter()
            .map(|m| m.id)
            .filter(|id| *id >= start && *id < end)
            .collect();
        let messages = self
            .messages
            .iter()
            .filter(|m| kept.contains(&m.id))
            .map(|m| NetraceMessage {
                id: m.id,
                src: m.src,
                dst: m.dst,
                size_class: m.size_class,
                deps: m
                    .deps
                    .iter()
                    .copied()
                    .filter(|d| kept.contains(d))
                    .collect(),
            })
            .collect();
        Self { messages }
    }
}

fn validate_acyclic(
    messages: &[NetraceMessage],
    ids: &HashMap<u64, usize>,
) -> Result<(), NetraceError> {
    // Kahn's algorithm over the dependency DAG (edge: dependency -> dependent).
    let mut indegree: Vec<usize> = messages.iter().map(|m| m.deps.len()).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); messages.len()];
    for (idx, m) in messages.iter().enumerate() {
        for dep in &m.deps {
            dependents[ids[dep]].push(idx);
        }
    }
    let mut queue: VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter(|(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();
    let mut processed = 0;
    while let Some(idx) = queue.pop_front() {
        processed += 1;
        for &dep_idx in &dependents[idx] {
            indegree[dep_idx] -= 1;
            if indegree[dep_idx] == 0 {
                queue.push_back(dep_idx);
            }
        }
    }
    if processed != messages.len() {
        let stuck = indegree
            .iter()
            .position(|&d| d > 0)
            .expect("processed < len implies some node was never reached");
        return Err(NetraceError::DependencyCycle {
            id: messages[stuck].id,
        });
    }
    Ok(())
}

/// How trace-native node ids (arbitrary, from whatever system captured the
/// trace) map onto this run's mesh node count (P3.5-F6, node-count remap).
#[derive(Clone, Copy, Debug)]
pub enum NodeMapping {
    /// Trace node ids are used as-is; out-of-range ids are an error.
    Identity,
    /// `trace_id % mesh_nodes`: a simple, deterministic way to fit a
    /// larger-or-smaller trace's node count onto this mesh. Multiple trace
    /// nodes can alias onto the same mesh node when the trace has more nodes
    /// than the mesh.
    Modulo,
}

impl NodeMapping {
    fn map(&self, id: u32, mesh_nodes: u32) -> Result<RouterId, NetraceError> {
        match self {
            NodeMapping::Identity => {
                if id < mesh_nodes {
                    Ok(RouterId(id))
                } else {
                    Err(NetraceError::NodeOutOfRange { id, mesh_nodes })
                }
            }
            NodeMapping::Modulo => Ok(RouterId(id % mesh_nodes)),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            NodeMapping::Identity => "identity",
            NodeMapping::Modulo => "modulo",
        }
    }
}

/// Trace/model identity + parameters (P3.5-F7): a human-readable stamp a
/// caller can fold into a run's provenance, mirroring
/// `tessera_params::ParamDb::provenance` (there is no run-record type to
/// write it into yet; see `docs/OUTSTANDING_PRD.md` OUT-17).
#[derive(Clone, Debug)]
pub struct TraceProvenance {
    pub source: String,
    pub region: Option<(u64, u64)>,
    pub node_mapping: NodeMapping,
}

impl TraceProvenance {
    pub fn stamp(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "# Trace provenance: TNI v1 '{}'", self.source);
        let _ = writeln!(s, "node_mapping = {}", self.node_mapping.label());
        match self.region {
            Some((start, end)) => {
                let _ = writeln!(s, "region = [{start}, {end})");
            }
            None => {
                let _ = writeln!(s, "region = full trace");
            }
        }
        s
    }
}

#[derive(Debug)]
struct MessageState {
    id: u64,
    src: RouterId,
    dst: RouterId,
    size: u16,
    remaining_preds: u32,
    dependents: Vec<usize>,
    /// This message's position among messages sharing its source, in trace
    /// file order: the sequence number [`ReorderBuffer`] reassembles on
    /// delivery (see the module doc for why this is a reporting aid, not a
    /// correctness dependency).
    src_seq: u64,
    injected: bool,
    delivered: bool,
}

#[derive(Debug)]
struct NetraceState {
    messages: Vec<MessageState>,
    /// Per (mapped) node: message indices ready to inject, in the order they
    /// became ready.
    ready: Vec<VecDeque<usize>>,
    rob: ReorderBuffer<u64>,
    /// Per source node, delivered message ids in true logical (trace) order,
    /// reassembled from however the network actually delivered them —
    /// demonstrates P3.5-F5 for a real generator.
    delivered_order: HashMap<RouterId, Vec<u64>>,
}

/// A [`TrafficGenerator`] that replays a [`NetraceTrace`]: a message is
/// injected at its source only once every dependency predecessor has been
/// delivered (P3.5-F1), preserving the trace's critical path. Pure replay —
/// no randomness is drawn, so the `rng` parameter of `on_tick` is unused.
#[derive(Debug)]
pub struct Netrace {
    state: Mutex<NetraceState>,
    provenance: TraceProvenance,
    /// Cap on messages injected per node per cycle; keeps injection
    /// deterministic and avoids a large simultaneous-ready-set (e.g. every
    /// dependency-free message at trace start) bursting in one cycle.
    max_inject_per_tick: u32,
}

impl Netrace {
    pub fn load(
        trace: &NetraceTrace,
        mesh_nodes: u32,
        mapping: NodeMapping,
        sizing: MessageSizing,
        provenance: TraceProvenance,
    ) -> Result<Self, NetraceError> {
        let mut src_seq_counter: HashMap<RouterId, u64> = HashMap::new();
        let mut messages = Vec::with_capacity(trace.messages.len());
        let mut ids = HashMap::with_capacity(trace.messages.len());
        for (idx, m) in trace.messages.iter().enumerate() {
            ids.insert(m.id, idx);
            let src = mapping.map(m.src, mesh_nodes)?;
            let dst = mapping.map(m.dst, mesh_nodes)?;
            let src_seq = *src_seq_counter
                .entry(src)
                .and_modify(|c| *c += 1)
                .or_insert(0);
            messages.push(MessageState {
                id: m.id,
                src,
                dst,
                size: sizing.flits(m.size_class),
                remaining_preds: m.deps.len() as u32,
                dependents: Vec::new(),
                src_seq,
                injected: false,
                delivered: false,
            });
        }
        for m in &trace.messages {
            let idx = ids[&m.id];
            for dep in &m.deps {
                messages[ids[dep]].dependents.push(idx);
            }
        }

        let mut ready = vec![VecDeque::new(); mesh_nodes as usize];
        for (idx, m) in messages.iter().enumerate() {
            if m.remaining_preds == 0 {
                ready[m.src.index()].push_back(idx);
            }
        }

        Ok(Self {
            state: Mutex::new(NetraceState {
                messages,
                ready,
                rob: ReorderBuffer::new(),
                delivered_order: HashMap::new(),
            }),
            provenance,
            max_inject_per_tick: 1,
        })
    }

    pub fn provenance(&self) -> &TraceProvenance {
        &self.provenance
    }

    /// Messages still awaiting delivery of at least one predecessor
    /// (progress observable for tests/diagnostics).
    pub fn outstanding_dependencies(&self) -> usize {
        let state = self.state.lock().unwrap();
        state
            .messages
            .iter()
            .filter(|m| !m.delivered)
            .filter(|m| m.remaining_preds > 0)
            .count()
    }

    /// Delivered message ids from `src`, reassembled into true trace order
    /// regardless of what order the network actually delivered them in.
    pub fn delivered_order(&self, src: RouterId) -> Vec<u64> {
        self.state
            .lock()
            .unwrap()
            .delivered_order
            .get(&src)
            .cloned()
            .unwrap_or_default()
    }
}

impl TrafficGenerator for Netrace {
    fn on_tick(&self, _cycle: u64, node: RouterId, _rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        let mut state = self.state.lock().unwrap();
        let cap = self.max_inject_per_tick as usize;
        for _ in 0..cap {
            let Some(&idx) = state.ready[node.index()].front() else {
                break;
            };
            state.ready[node.index()].pop_front();
            let m = &mut state.messages[idx];
            debug_assert!(!m.injected, "a ready message is injected at most once");
            m.injected = true;
            out.push(PacketDesc {
                dst: m.dst,
                size: m.size,
                tag: idx as u64,
            });
        }
    }

    fn on_deliver(&self, _cycle: u64, delivery: Delivery) {
        let mut state = self.state.lock().unwrap();
        let idx = delivery.tag as usize;
        debug_assert!(!state.messages[idx].delivered, "message delivered twice");
        state.messages[idx].delivered = true;

        let (src, seq, id) = {
            let m = &state.messages[idx];
            (m.src, m.src_seq, m.id)
        };
        if let Ok(in_order) = state.rob.arrive(src, seq, id) {
            state
                .delivered_order
                .entry(src)
                .or_default()
                .extend(in_order);
        }

        let dependents = state.messages[idx].dependents.clone();
        for dep_idx in dependents {
            let dep = &mut state.messages[dep_idx];
            dep.remaining_preds -= 1;
            if dep.remaining_preds == 0 {
                let dep_src = dep.src;
                state.ready[dep_src.index()].push_back(dep_idx);
            }
        }
    }

    fn snapshot(&self) -> Option<Vec<u8>> {
        let state = self.state.lock().unwrap();
        let snap = NetraceStateSnapshot {
            messages: state
                .messages
                .iter()
                .map(|m| MessageMutableSnapshot {
                    remaining_preds: m.remaining_preds,
                    injected: m.injected,
                    delivered: m.delivered,
                })
                .collect(),
            ready: state
                .ready
                .iter()
                .map(|q| q.iter().map(|&i| i as u32).collect())
                .collect(),
            rob_sources: state
                .rob
                .snapshot()
                .into_iter()
                .map(|(src, next, pending)| (src.0, next, pending))
                .collect(),
            delivered_order: state
                .delivered_order
                .iter()
                .map(|(src, ids)| (src.0, ids.clone()))
                .collect(),
        };
        // A hand-rolled `Vec<u8>`/`u32`/`u64`/`bool` DTO always encodes.
        Some(bincode::serialize(&snap).expect("NetraceStateSnapshot always serializes"))
    }

    fn restore(&self, bytes: &[u8]) {
        // `bytes` comes from this same generator's `snapshot` via a keyframe
        // whose schema/dimensions the caller (`Simulation::restore`) has
        // already validated against this run, so a decode failure here would
        // indicate corrupt storage, not a reachable input this trait needs to
        // report as a `Result`.
        let snap: NetraceStateSnapshot =
            bincode::deserialize(bytes).expect("valid Netrace snapshot bytes");
        let mut state = self.state.lock().unwrap();
        for (m, snap_m) in state.messages.iter_mut().zip(snap.messages) {
            m.remaining_preds = snap_m.remaining_preds;
            m.injected = snap_m.injected;
            m.delivered = snap_m.delivered;
        }
        state.ready = snap
            .ready
            .into_iter()
            .map(|q| q.into_iter().map(|i| i as usize).collect())
            .collect();
        state.rob.restore(
            snap.rob_sources
                .into_iter()
                .map(|(src, next, pending)| (RouterId(src), next, pending))
                .collect(),
        );
        state.delivered_order = snap
            .delivered_order
            .into_iter()
            .map(|(src, ids)| (RouterId(src), ids))
            .collect();
    }
}

/// The subset of [`MessageState`] that changes after [`Netrace::load`]: the
/// rest (`id`/`src`/`dst`/`size`/`dependents`/`src_seq`) is re-derived
/// identically from the trace every load, so only this needs to travel.
#[derive(Serialize, Deserialize)]
struct MessageMutableSnapshot {
    remaining_preds: u32,
    injected: bool,
    delivered: bool,
}

/// One `ReorderBuffer::snapshot()` source entry with `RouterId` lowered to a
/// bare `u32` (source node, next-expected seq, buffered `(seq, message id)`
/// pairs).
type RobSourceWire = (u32, u64, Vec<(u64, u64)>);

/// Wire form of [`NetraceState`] for [`TrafficGenerator::snapshot`]/`restore`
/// (P4-F1): `RouterId`s travel as bare `u32`s so this crate doesn't need
/// `tessera-core`'s `snapshot` feature just for this DTO.
#[derive(Serialize, Deserialize)]
struct NetraceStateSnapshot {
    messages: Vec<MessageMutableSnapshot>,
    /// Per (mapped) node: ready-but-not-yet-injected message indices, in order.
    ready: Vec<Vec<u32>>,
    rob_sources: Vec<RobSourceWire>,
    delivered_order: Vec<(u32, Vec<u64>)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::RngStreams;

    fn sizing() -> MessageSizing {
        MessageSizing {
            control_flits: 1,
            data_flits: 4,
        }
    }

    fn provenance(source: &str) -> TraceProvenance {
        TraceProvenance {
            source: source.to_string(),
            region: None,
            node_mapping: NodeMapping::Identity,
        }
    }

    #[test]
    fn parses_a_well_formed_trace() {
        let trace = NetraceTrace::parse(
            "version 1\n\
             # a producer/consumer pair\n\
             0 0 3 control -\n\
             1 0 3 data 0\n",
        )
        .unwrap();
        assert_eq!(trace.messages.len(), 2);
        assert_eq!(trace.messages[1].deps, vec![0]);
    }

    #[test]
    fn rejects_unknown_dependency() {
        let err = NetraceTrace::parse("version 1\n0 0 1 control 99\n").unwrap_err();
        assert_eq!(err, NetraceError::UnknownDependency { id: 0, dep: 99 });
    }

    #[test]
    fn rejects_dependency_cycles() {
        let err = NetraceTrace::parse("version 1\n0 0 1 control 1\n1 1 0 control 0\n").unwrap_err();
        assert!(matches!(err, NetraceError::DependencyCycle { .. }));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let err = NetraceTrace::parse("version 1\n0 0 1 control -\n0 1 2 control -\n").unwrap_err();
        assert_eq!(err, NetraceError::DuplicateId { id: 0, line: 3 });
    }

    #[test]
    fn requires_a_version_line() {
        let err = NetraceTrace::parse("0 0 1 control -\n").unwrap_err();
        assert!(matches!(err, NetraceError::Malformed { .. }));
    }

    #[test]
    fn a_message_is_not_ready_until_all_predecessors_delivered() {
        // 2 depends on both 0 and 1; must not be injected until both deliver.
        let trace = NetraceTrace::parse(
            "version 1\n\
             0 0 5 control -\n\
             1 1 5 control -\n\
             2 5 9 data 0,1\n",
        )
        .unwrap();
        let gen =
            Netrace::load(&trace, 16, NodeMapping::Identity, sizing(), provenance("t")).unwrap();
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();

        // Node 5 (message 2's source) has nothing ready yet.
        gen.on_tick(0, RouterId(5), &mut rng, &mut out);
        assert!(out.is_empty());

        gen.on_deliver(
            1,
            Delivery {
                src: RouterId(0),
                dst: RouterId(5),
                size: 1,
                tag: 0,
                inject_cycle: 0,
            },
        );
        out.clear();
        gen.on_tick(2, RouterId(5), &mut rng, &mut out);
        assert!(out.is_empty(), "message 1 hasn't delivered yet");

        gen.on_deliver(
            3,
            Delivery {
                src: RouterId(1),
                dst: RouterId(5),
                size: 1,
                tag: 1,
                inject_cycle: 0,
            },
        );
        out.clear();
        gen.on_tick(4, RouterId(5), &mut rng, &mut out);
        assert_eq!(
            out.len(),
            1,
            "both predecessors delivered: message 2 is ready"
        );
        assert_eq!(out[0].dst, RouterId(9));
        assert_eq!(out[0].tag, 2);
    }

    #[test]
    fn dependency_free_messages_are_ready_immediately() {
        let trace = NetraceTrace::parse("version 1\n0 0 1 control -\n").unwrap();
        let gen =
            Netrace::load(&trace, 4, NodeMapping::Identity, sizing(), provenance("t")).unwrap();
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();
        gen.on_tick(0, RouterId(0), &mut rng, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].dst, RouterId(1));
        assert_eq!(out[0].size, 1);
    }

    #[test]
    fn region_selection_drops_out_of_region_dependencies() {
        let trace = NetraceTrace::parse("version 1\n0 0 1 control -\n1 0 1 data 0\n2 0 1 data 1\n")
            .unwrap();
        let region = trace.select_region(1, 3);
        assert_eq!(region.messages.len(), 2);
        // message 1 depended on 0, which fell outside the region: treated as satisfied.
        assert_eq!(region.messages[0].deps, Vec::<u64>::new());
        assert_eq!(region.messages[1].deps, vec![1]);
    }

    #[test]
    fn modulo_node_mapping_wraps_out_of_range_ids() {
        let trace = NetraceTrace::parse("version 1\n0 10 11 control -\n").unwrap();
        let gen = Netrace::load(&trace, 4, NodeMapping::Modulo, sizing(), provenance("t")).unwrap();
        let mut rng = RngStreams::new(1).pe(2);
        let mut out = Vec::new();
        gen.on_tick(0, RouterId(2), &mut rng, &mut out); // 10 % 4 == 2
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].dst, RouterId(3)); // 11 % 4 == 3
    }

    #[test]
    fn identity_mapping_rejects_out_of_range_nodes() {
        let trace = NetraceTrace::parse("version 1\n0 10 1 control -\n").unwrap();
        let err =
            Netrace::load(&trace, 4, NodeMapping::Identity, sizing(), provenance("t")).unwrap_err();
        assert_eq!(
            err,
            NetraceError::NodeOutOfRange {
                id: 10,
                mesh_nodes: 4
            }
        );
    }

    #[test]
    fn delivered_order_reassembles_out_of_network_order_deliveries() {
        // Three independent (no-dep) messages from the same source; deliver
        // them to the generator out of trace order and confirm the reported
        // order is the true trace order, not delivery order.
        let trace =
            NetraceTrace::parse("version 1\n0 0 1 control -\n1 0 1 control -\n2 0 1 control -\n")
                .unwrap();
        let gen =
            Netrace::load(&trace, 4, NodeMapping::Identity, sizing(), provenance("t")).unwrap();
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();
        // Drain the ready queue in trace order to learn each message's tag
        // (== internal index, which equals trace order here).
        for _ in 0..3 {
            out.clear();
            gen.on_tick(0, RouterId(0), &mut rng, &mut out);
            assert_eq!(out.len(), 1);
        }
        let deliver = |tag: u64| {
            gen.on_deliver(
                0,
                Delivery {
                    src: RouterId(0),
                    dst: RouterId(1),
                    size: 1,
                    tag,
                    inject_cycle: 0,
                },
            )
        };
        deliver(2);
        deliver(0);
        deliver(1);
        assert_eq!(gen.delivered_order(RouterId(0)), vec![0, 1, 2]);
    }

    #[test]
    fn snapshot_and_restore_round_trips_mid_replay_state() {
        // Same fan-in trace as `a_message_is_not_ready_until_all_predecessors_delivered`.
        let trace = NetraceTrace::parse(
            "version 1\n\
             0 0 5 control -\n\
             1 1 5 control -\n\
             2 5 9 data 0,1\n",
        )
        .unwrap();
        let gen =
            Netrace::load(&trace, 16, NodeMapping::Identity, sizing(), provenance("t")).unwrap();

        // Deliver message 0 only; message 2 still awaits message 1.
        gen.on_deliver(
            1,
            Delivery {
                src: RouterId(0),
                dst: RouterId(5),
                size: 1,
                tag: 0,
                inject_cycle: 0,
            },
        );
        assert_eq!(gen.outstanding_dependencies(), 1);

        let bytes = gen.snapshot().expect("Netrace has stateful snapshot");

        // Restore onto a fresh generator built the same way `SimSource`
        // reconstruction would (from the same trace), not the original.
        let restored =
            Netrace::load(&trace, 16, NodeMapping::Identity, sizing(), provenance("t")).unwrap();
        restored.restore(&bytes);
        assert_eq!(restored.outstanding_dependencies(), 1);

        // Deliver message 1 on both; message 2 must become ready identically.
        let deliver_msg1 = |g: &Netrace| {
            g.on_deliver(
                3,
                Delivery {
                    src: RouterId(1),
                    dst: RouterId(5),
                    size: 1,
                    tag: 1,
                    inject_cycle: 0,
                },
            )
        };
        deliver_msg1(&gen);
        deliver_msg1(&restored);

        let rng = RngStreams::new(1).pe(5);
        let mut out_gen = Vec::new();
        let mut out_restored = Vec::new();
        gen.on_tick(4, RouterId(5), &mut rng.clone(), &mut out_gen);
        restored.on_tick(4, RouterId(5), &mut rng.clone(), &mut out_restored);

        assert_eq!(
            out_gen.len(),
            1,
            "message 2 should be ready after both deps deliver"
        );
        assert_eq!(out_restored.len(), 1);
        assert_eq!(out_gen[0].dst, out_restored[0].dst);
        assert_eq!(out_gen[0].size, out_restored[0].size);
        assert_eq!(out_gen[0].tag, out_restored[0].tag);
    }
}
