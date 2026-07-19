//! Phase 3.5 traffic realism, exercised over a real `Simulation` (not just
//! unit-level `on_tick`/`on_deliver` calls): Netrace dependency enforcement
//! survives real routing/allocation/link-latency, SynFull composes as an
//! ordinary `TrafficGenerator` with no core change (P3.5-N3), and both are
//! deterministic under a fixed seed (P3.5-N1).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tessera_core::{
    Delivery, NodeRng, PacketDesc, RouterId, SimConfig, Simulation, TrafficGenerator,
};
use tessera_model::Mesh2D;
use tessera_routing::{DimensionOrderXy, FirstAvailable, RoundRobin};
use tessera_traffic::netrace::{
    MessageSizing, Netrace, NetraceTrace, NodeMapping, TraceProvenance,
};
use tessera_traffic::synfull::{ModelProvenance, SynFull, SynFullModel};

/// Records, for every packet tag, the cycle it was injected and the cycle it
/// was delivered, without altering the wrapped generator's behavior.
struct Spy<G> {
    inner: G,
    injected: Arc<Mutex<HashMap<u64, u64>>>,
    delivered: Arc<Mutex<HashMap<u64, u64>>>,
}

impl<G: TrafficGenerator> TrafficGenerator for Spy<G> {
    fn on_tick(&self, cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        let start = out.len();
        self.inner.on_tick(cycle, node, rng, out);
        let mut log = self.injected.lock().unwrap();
        for desc in &out[start..] {
            log.entry(desc.tag).or_insert(cycle);
        }
    }

    fn on_deliver(&self, cycle: u64, delivery: Delivery) {
        self.delivered
            .lock()
            .unwrap()
            .entry(delivery.tag)
            .or_insert(cycle);
        self.inner.on_deliver(cycle, delivery);
    }
}

fn sizing() -> MessageSizing {
    MessageSizing {
        control_flits: 1,
        data_flits: 4,
    }
}

fn mesh_cfg(seed: u64) -> SimConfig {
    SimConfig {
        buffer_depth: 8,
        num_vcs: 2,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed,
    }
}

/// Node 5 fans in from nodes 0 and 1, then forwards to node 10: message 2
/// must not be injected before *both* of its producers have delivered.
fn fan_in_trace() -> NetraceTrace {
    NetraceTrace::parse(
        "version 1\n\
         0 0 5 control -\n\
         1 1 5 control -\n\
         2 5 10 data 0,1\n",
    )
    .unwrap()
}

fn run_netrace(seed: u64) -> (HashMap<u64, u64>, HashMap<u64, u64>) {
    let trace = fan_in_trace();
    let gen = Netrace::load(
        &trace,
        16,
        NodeMapping::Identity,
        sizing(),
        TraceProvenance {
            source: "inline fan-in".to_string(),
            region: None,
            node_mapping: NodeMapping::Identity,
        },
    )
    .unwrap();

    let injected = Arc::new(Mutex::new(HashMap::new()));
    let delivered = Arc::new(Mutex::new(HashMap::new()));
    let spy = Spy {
        inner: gen,
        injected: injected.clone(),
        delivered: delivered.clone(),
    };

    let mut sim = Simulation::new(
        mesh_cfg(seed),
        Box::new(Mesh2D::new(4, 4)),
        Box::new(DimensionOrderXy),
        Box::new(FirstAvailable),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(spy),
    );
    for _ in 0..500 {
        sim.tick();
    }

    let injected = injected.lock().unwrap().clone();
    let delivered = delivered.lock().unwrap().clone();
    (injected, delivered)
}

#[test]
fn netrace_dependencies_are_enforced_over_a_real_simulation() {
    let (injected, delivered) = run_netrace(1);

    assert_eq!(
        delivered.len(),
        3,
        "all three messages should be delivered: {delivered:?}"
    );
    let msg2_inject = injected[&2];
    assert!(
        msg2_inject > delivered[&0],
        "message 2 (deps 0,1) must inject strictly after message 0 delivers"
    );
    assert!(
        msg2_inject > delivered[&1],
        "message 2 (deps 0,1) must inject strictly after message 1 delivers"
    );
}

#[test]
fn netrace_replay_is_deterministic_under_a_fixed_seed() {
    let (injected_a, delivered_a) = run_netrace(3);
    let (injected_b, delivered_b) = run_netrace(3);
    assert_eq!(injected_a, injected_b);
    assert_eq!(delivered_a, delivered_b);
}

const SIMPLE_SYNFULL_MODEL: &str = r#"
    name = "cli-smoke-test"
    initial_state = "a"

    [[states]]
    name = "a"
    inject_prob = 0.3
    size_class = "control"
    dest = { kind = "uniform" }
    next = [ { state = "a", prob = 1.0 } ]
"#;

#[test]
fn synfull_composes_as_an_ordinary_traffic_generator() {
    // P3.5-N3: no core change is needed to run a SynFull source; it plugs
    // into `Simulation::new` exactly like `Uniform`/`Hotspot`.
    let model = SynFullModel::from_toml(SIMPLE_SYNFULL_MODEL, 16).unwrap();
    let gen = SynFull::new(
        model,
        16,
        sizing(),
        ModelProvenance {
            source: "inline".to_string(),
        },
    );

    let mut sim = Simulation::new(
        mesh_cfg(9),
        Box::new(Mesh2D::new(4, 4)),
        Box::new(DimensionOrderXy),
        Box::new(FirstAvailable),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(gen),
    );
    for _ in 0..500 {
        sim.tick();
    }

    assert!(
        sim.metrics.injected_packets > 0,
        "SynFull traffic should have injected packets"
    );
    assert!(
        sim.metrics.ejected_packets > 0,
        "SynFull traffic should have flowed and been delivered"
    );
}
