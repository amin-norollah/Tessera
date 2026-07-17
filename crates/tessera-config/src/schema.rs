//! The runtime config schema: a `serde` mirror of [`tessera_core::SimConfig`]
//! plus the component selection (routing/selection/allocator/traffic, by
//! string key) that [`crate::validate::validate`] checks before a run starts.
//!
//! Field names follow BookSim/Noxim conventions where sensible (`num_vcs`,
//! `buffer_depth`, `*_delay`) per `INITIAL_PRD.md` §20.13, so the schema reads
//! familiarly to anyone coming from those tools.

use serde::{Deserialize, Serialize};

/// The current schema version. Bump on a breaking schema change; `validate`
/// rejects a file whose `version` it doesn't recognize.
pub const CURRENT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimConfigFile {
    pub version: u32,
    pub topology: TopologyCfg,
    pub router: RouterCfg,
    pub traffic: TrafficCfg,
    pub seed: u64,
    pub warmup: u64,
    pub measure: u64,
}

impl Default for SimConfigFile {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            topology: TopologyCfg::Mesh2D {
                width: 8,
                height: 8,
            },
            router: RouterCfg::default(),
            traffic: TrafficCfg::default(),
            seed: 1,
            warmup: 2000,
            measure: 10_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TopologyCfg {
    Mesh2D { width: u16, height: u16 },
    Torus2D { width: u16, height: u16 },
}

impl TopologyCfg {
    pub fn dimensions(&self) -> (u16, u16) {
        match *self {
            TopologyCfg::Mesh2D { width, height } => (width, height),
            TopologyCfg::Torus2D { width, height } => (width, height),
        }
    }

    pub fn is_torus(&self) -> bool {
        matches!(self, TopologyCfg::Torus2D { .. })
    }
}

/// Per-router structural + strategy-selection config. `routing`/`selection`
/// name components by their [`tessera_plugin::Registry`] key; `validate`
/// checks the string against known conventions (e.g. an "adaptive" key
/// implies fully-adaptive routing) rather than depending on the plugin crate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouterCfg {
    pub num_vcs: u8,
    /// VC partition for Duato escape-VC routing. `None` means every VC is
    /// available to every routing decision (no escape class carved out).
    pub vc_classes: Option<VcClassCfg>,
    pub buffer_depth: u16,
    pub router_latency: u16,
    pub link_latency: u16,
    pub credit_delay: u16,
    pub routing: String,
    pub selection: String,
    pub allocator_va: AllocatorCfg,
    pub allocator_sa: AllocatorCfg,
    /// Arbitration iterations per cycle for VA/SA (iSLIP-style multi-round
    /// matching); 1 is a single request/grant/accept pass.
    pub alloc_iters: u8,
    /// Whether the topology's dateline links switch a crossing packet to the
    /// upper VC half (torus deadlock avoidance, P1-F12).
    pub dateline: bool,
}

impl Default for RouterCfg {
    fn default() -> Self {
        Self {
            num_vcs: 1,
            vc_classes: None,
            buffer_depth: 4,
            router_latency: 1,
            link_latency: 1,
            credit_delay: 1,
            routing: "xy".to_string(),
            selection: "first-available".to_string(),
            allocator_va: AllocatorCfg::RoundRobin,
            allocator_sa: AllocatorCfg::RoundRobin,
            alloc_iters: 1,
            dateline: false,
        }
    }
}

/// VCs reserved for the escape (always-DOR, deadlock-free) class vs the
/// adaptive class, for fully-adaptive + Duato routing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VcClassCfg {
    pub escape: u8,
    pub adaptive: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocatorCfg {
    RoundRobin,
    Matrix,
    ISlip,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrafficCfg {
    pub kind: TrafficKind,
    pub packet_size: PacketSizeCfg,
}

impl Default for TrafficCfg {
    fn default() -> Self {
        Self {
            kind: TrafficKind::Uniform {
                injection_rate: 0.1,
            },
            packet_size: PacketSizeCfg::Fixed { size: 4 },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrafficKind {
    Uniform {
        injection_rate: f64,
    },
    /// A Noxim traffic table imported via [`crate::noxim::parse_traffic_table`];
    /// `path` is resolved by the caller (this crate only parses table text).
    TableBased {
        path: String,
    },
    /// A Netrace-format dependency trace (`tessera_traffic::netrace`,
    /// P3.5-F1/F7); `path` is resolved by the caller, which loads it with
    /// `tessera_traffic::netrace::NetraceTrace::from_reader` and builds the
    /// generator with `Netrace::load`. `region` selects `[start, end)` by
    /// message id (`None` replays the full trace, P3.5-F1 region selection).
    Netrace {
        path: String,
        #[serde(default)]
        region: Option<(u64, u64)>,
        #[serde(default = "NodeMappingCfg::default_for_traffic")]
        node_mapping: NodeMappingCfg,
    },
    /// A SynFull-style Markov traffic model (`tessera_traffic::synfull`,
    /// P3.5-F3); `model_path` is resolved by the caller, which loads it with
    /// `SynFullModel::from_toml` and builds the generator with `SynFull::new`.
    SynFull {
        model_path: String,
    },
}

/// How a trace's node ids remap onto this run's mesh (P3.5-F6), mirroring
/// `tessera_traffic::netrace::NodeMapping`. Kept as separate, plain-data
/// schema types rather than depending on `tessera-traffic` directly: this
/// crate's schema stays decoupled from concrete generator implementations
/// (see the module doc), the same reason `TableBased`'s table text is parsed
/// here but built into a router elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeMappingCfg {
    Identity,
    Modulo,
}

impl NodeMappingCfg {
    fn default_for_traffic() -> Self {
        NodeMappingCfg::Identity
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PacketSizeCfg {
    Fixed {
        size: u16,
    },
    /// Two size classes drawn per packet (e.g. short control vs long data);
    /// `data_ratio` is the fraction of packets drawn as `data`-sized.
    Bimodal {
        control: u16,
        data: u16,
        data_ratio: f64,
    },
}

/// Routing keys this schema recognizes as dimension-order (deadlock-free by
/// construction on a mesh with 1 VC; on a torus they need dateline VCs).
pub fn is_dimension_order_key(routing: &str) -> bool {
    matches!(routing, "xy" | "xyz")
}

/// Routing keys this schema recognizes as fully-adaptive (Duato), which
/// require an escape-VC class to stay deadlock-free.
pub fn is_adaptive_key(routing: &str) -> bool {
    routing == "adaptive"
}
