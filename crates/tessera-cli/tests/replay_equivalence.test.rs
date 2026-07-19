//! Phase 4 replay-equivalence (gate G4.1/G4.2), exercised over a real
//! `Simulation`: re-simulating from a fresh `tessera_record::SimSource` and
//! seeking via a mid-run keyframe both reproduce a reference run's exact
//! event trace, across a matrix of traffic/routing sources: deterministic
//! routing, adaptive/RNG-driven selection (stresses `router_rng` stream
//! restore), Netrace, and SynFull (both stress traffic-generator state
//! restore, since their internal state isn't derivable from RNG alone; see
//! `TrafficGenerator::snapshot`/`restore`).

use tessera_core::{SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_record::{
    first_divergence, run_collecting_events, KeyframeReader, KeyframeWriter, SeekEngine, SimSource,
};
use tessera_routing::{DimensionOrderXy, FirstAvailable, OddEven, Random, RoundRobin};
use tessera_traffic::netrace::{
    MessageSizing, Netrace, NetraceTrace, NodeMapping, TraceProvenance,
};
use tessera_traffic::synfull::{ModelProvenance, SynFull, SynFullModel};
use tessera_traffic::Uniform;

fn mesh_cfg(seed: u64, num_vcs: u8) -> SimConfig {
    SimConfig {
        buffer_depth: 8,
        num_vcs,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed,
    }
}

fn sizing() -> MessageSizing {
    MessageSizing {
        control_flits: 1,
        data_flits: 4,
    }
}

/// Deterministic dimension-order routing + round-robin everything: the
/// baseline scenario.
struct UniformXySource {
    cfg: SimConfig,
}

impl SimSource for UniformXySource {
    fn fresh(&self) -> Simulation {
        Simulation::new(
            self.cfg,
            Box::new(Mesh2D::new(4, 4)),
            Box::new(DimensionOrderXy),
            Box::new(FirstAvailable),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(Uniform::new(16, 0.3, self.cfg.packet_size)),
        )
    }
}

/// Adaptive routing (`OddEven`) + RNG-driven selection (`Random`): stresses
/// `router_rng` stream restore, unlike the deterministic baseline.
struct AdaptiveSource {
    cfg: SimConfig,
}

impl SimSource for AdaptiveSource {
    fn fresh(&self) -> Simulation {
        Simulation::new(
            self.cfg,
            Box::new(Mesh2D::new(4, 4)),
            Box::new(OddEven),
            Box::new(Random),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(Uniform::new(16, 0.3, self.cfg.packet_size)),
        )
    }
}

/// A fan-in dependency trace with independent messages mixed in, so both
/// dependency-gated and ungated injection are exercised.
fn fan_in_trace() -> NetraceTrace {
    NetraceTrace::parse(
        "version 1\n\
         0 0 5 control -\n\
         1 1 5 control -\n\
         2 5 10 data 0,1\n\
         3 2 11 control -\n\
         4 3 12 control -\n\
         5 11 13 control 3\n",
    )
    .unwrap()
}

/// Netrace's dependency ready-set/reorder-buffer state is path-dependent
/// (driven by delivery history, not RNG): this stresses `TrafficGenerator::
/// snapshot`/`restore`, not just RNG-stream restore.
struct NetraceSource {
    cfg: SimConfig,
}

impl SimSource for NetraceSource {
    fn fresh(&self) -> Simulation {
        let trace = fan_in_trace();
        let gen = Netrace::load(
            &trace,
            16,
            NodeMapping::Identity,
            sizing(),
            TraceProvenance {
                source: "replay-equivalence test".to_string(),
                region: None,
                node_mapping: NodeMapping::Identity,
            },
        )
        .unwrap();
        Simulation::new(
            self.cfg,
            Box::new(Mesh2D::new(4, 4)),
            Box::new(DimensionOrderXy),
            Box::new(FirstAvailable),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(gen),
        )
    }
}

const SYNFULL_MODEL: &str = r#"
    name = "replay-equivalence"
    initial_state = "a"

    [[states]]
    name = "a"
    inject_prob = 0.3
    size_class = "control"
    dest = { kind = "uniform" }
    next = [ { state = "a", prob = 1.0 } ]
"#;

/// SynFull's per-node Markov index is path-dependent (advanced by draws
/// already made): like Netrace, this stresses `TrafficGenerator::snapshot`/
/// `restore` beyond what restoring `pe_rng` alone would reconstruct.
struct SynFullSource {
    cfg: SimConfig,
}

impl SimSource for SynFullSource {
    fn fresh(&self) -> Simulation {
        let model = SynFullModel::from_toml(SYNFULL_MODEL, 16).unwrap();
        let gen = SynFull::new(
            model,
            16,
            sizing(),
            ModelProvenance {
                source: "inline".to_string(),
            },
        );
        Simulation::new(
            self.cfg,
            Box::new(Mesh2D::new(4, 4)),
            Box::new(DimensionOrderXy),
            Box::new(FirstAvailable),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(gen),
        )
    }
}

/// The gate: a from-seed re-simulation (P4-F3) and a nearest-keyframe replay
/// (P4-F2) both reproduce a reference run's exact event trace to `until`,
/// bit-for-bit: not just matching summary metrics.
fn assert_replay_is_equivalent(source: &impl SimSource, until: u64, keyframe_cycle: u64) {
    assert!(keyframe_cycle < until);

    // Reference run: ticks straight through, recording every event and
    // snapshotting at `keyframe_cycle`.
    let mut reference = source.fresh();
    reference.record_events(true);
    let mut reference_events = Vec::new();
    let mut keyframe = None;
    let mut keyframe_event_count = 0;
    while reference.cycle < until {
        if reference.cycle == keyframe_cycle {
            keyframe = Some(reference.snapshot());
            keyframe_event_count = reference_events.len();
        }
        reference.tick();
        reference_events.extend(reference.take_events());
    }
    let keyframe = keyframe.expect("keyframe_cycle must be reached before `until`");
    assert!(
        !reference_events.is_empty(),
        "test scenario produced no events; the assertion below would be vacuous"
    );

    // Re-simulation from seed (P4-F3): a completely fresh sim, ticked the
    // same number of cycles, reproduces the exact same trace.
    let mut resim = source.fresh();
    let resim_events = run_collecting_events(&mut resim, until);
    assert_eq!(
        first_divergence(&reference_events, &resim_events),
        None,
        "resim-from-seed diverged from the reference run"
    );
    assert_eq!(resim.cycle, reference.cycle);
    assert_eq!(
        resim.metrics.injected_packets,
        reference.metrics.injected_packets
    );
    assert_eq!(
        resim.metrics.ejected_packets,
        reference.metrics.ejected_packets
    );

    // Keyframe restore + replay (P4-F2): overlay the mid-run keyframe onto a
    // fresh sim and tick forward; the tail trace (from `keyframe_cycle` to
    // `until`) must match the reference's tail exactly.
    let mut from_keyframe = source.fresh();
    from_keyframe
        .restore(keyframe)
        .expect("restore onto a freshly-built, same-shaped simulation must succeed");
    assert_eq!(
        from_keyframe.cycle, keyframe_cycle,
        "restore must reproduce the exact cycle the keyframe was taken at"
    );
    let tail_events = run_collecting_events(&mut from_keyframe, until);
    let reference_tail = &reference_events[keyframe_event_count..];
    assert_eq!(
        first_divergence(reference_tail, &tail_events),
        None,
        "keyframe-restore replay diverged from the reference run's tail"
    );
    assert_eq!(from_keyframe.cycle, reference.cycle);
    assert_eq!(
        from_keyframe.metrics.injected_packets,
        reference.metrics.injected_packets
    );
    assert_eq!(
        from_keyframe.metrics.ejected_packets,
        reference.metrics.ejected_packets
    );
}

#[test]
fn deterministic_routing_replay_is_equivalent() {
    let source = UniformXySource {
        cfg: mesh_cfg(7, 2),
    };
    assert_replay_is_equivalent(&source, 400, 150);
}

#[test]
fn adaptive_routing_replay_is_equivalent() {
    // Exercises `router_rng` restore: `Random` selection draws a tie-break
    // every time `OddEven` admits more than one candidate port.
    let source = AdaptiveSource {
        cfg: mesh_cfg(11, 2),
    };
    assert_replay_is_equivalent(&source, 400, 150);
}

#[test]
fn netrace_replay_is_equivalent() {
    let source = NetraceSource {
        cfg: mesh_cfg(3, 2),
    };
    assert_replay_is_equivalent(&source, 300, 80);
}

#[test]
fn synfull_replay_is_equivalent() {
    let source = SynFullSource {
        cfg: mesh_cfg(9, 2),
    };
    assert_replay_is_equivalent(&source, 300, 120);
}

#[test]
fn seek_via_a_disk_keyframe_matches_resimulation() {
    let source = UniformXySource {
        cfg: mesh_cfg(21, 2),
    };
    let dir = std::env::temp_dir().join(format!(
        "tessera_cli_replay_equivalence_test_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let writer = KeyframeWriter::create(&dir).unwrap();
    let mut sim = source.fresh();
    let keyframe_interval = 50;
    let until = 300;
    while sim.cycle < until {
        if sim.cycle.is_multiple_of(keyframe_interval) {
            writer.write(&sim.snapshot()).unwrap();
        }
        sim.tick();
    }

    let reader = KeyframeReader::open(&dir).unwrap();
    let engine = SeekEngine::with_keyframes(&source, &reader);

    for &n in &[10u64, 60, 175, 260] {
        let seeked = engine.seek_to(n).unwrap();
        let resimmed = engine.resimulate(n);
        assert_eq!(seeked.cycle, resimmed.cycle);
        assert_eq!(
            seeked.metrics.injected_packets, resimmed.metrics.injected_packets,
            "seek_to({n}) should match a from-seed resimulation to the same cycle"
        );
        assert_eq!(
            seeked.metrics.ejected_packets,
            resimmed.metrics.ejected_packets
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}
