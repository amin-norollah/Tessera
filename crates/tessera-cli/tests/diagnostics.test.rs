//! End-to-end diagnostics (P3-F15/F16): diagnostics are built from the sim's
//! observable state (`network_activity`, metrics, `router_occupancy`). A healthy
//! run trips no detector and conserves every flit; a frozen network snapshot
//! trips the deadlock detector.

use tessera_cli::build_sim;
use tessera_core::{SimConfig, Simulation};
use tessera_diag::{
    DeadlockDetector, Detector, HealthSample, InvariantChecker, LivelockDetector, Violation,
};

fn cfg(seed: u64) -> SimConfig {
    SimConfig {
        buffer_depth: 4,
        num_vcs: 2,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed,
    }
}

/// Build a diagnostic sample from the simulation's observable state.
fn sample(sim: &Simulation) -> HealthSample {
    HealthSample {
        cycle: sim.cycle,
        delivered_flits: sim.metrics.ejected_flits,
        total_hops: sim.metrics.total_hops,
        injected_flits: sim.metrics.injected_flits,
        dropped_flits: 0, // lossless kernel
        in_flight_flits: sim.in_flight(),
        router_occupancy: sim.router_occupancy(),
        flow_pending: Vec::new(),
        flow_granted: Vec::new(),
    }
}

#[test]
fn healthy_run_trips_no_detector_and_conserves_flits() {
    let mut sim = build_sim(6, 6, cfg(3), 0.15);
    sim.set_injecting(true);

    let mut deadlock = DeadlockDetector::new(300);
    let mut livelock = LivelockDetector::new(300);
    let mut invariants = InvariantChecker::new();

    for _ in 0..6000 {
        sim.tick();
        if sim.cycle.is_multiple_of(100) {
            let s = sample(&sim);
            assert!(
                deadlock.observe(&s).is_empty(),
                "no deadlock on healthy XY run"
            );
            assert!(
                livelock.observe(&s).is_empty(),
                "no livelock on healthy XY run"
            );
            let violations = invariants.observe(&s);
            assert!(
                violations.is_empty(),
                "flit conservation holds: {violations:?}"
            );
        }
    }
    assert!(sim.metrics.ejected_flits > 0, "traffic actually flowed");
}

#[test]
fn router_occupancy_is_a_valid_fraction() {
    let mut sim = build_sim(4, 4, cfg(11), 0.3);
    sim.set_injecting(true);
    for _ in 0..1500 {
        sim.tick();
    }
    let occ = sim.router_occupancy();
    assert_eq!(occ.len(), sim.num_nodes() as usize);
    assert!(occ.iter().all(|&o| (0.0..=1.0).contains(&o)), "{occ:?}");
    // Under load, at least one router holds some flits.
    assert!(occ.iter().any(|&o| o > 0.0));
}

#[test]
fn invariant_checker_catches_an_injected_flit_loss() {
    let mut sim = build_sim(4, 4, cfg(5), 0.2);
    sim.set_injecting(true);
    for _ in 0..1000 {
        sim.tick();
    }
    let mut s = sample(&sim);
    // The real observation conserves flits: injected == delivered + in_flight.
    assert!(InvariantChecker::check_flit_conservation(&s).is_ok());
    // Inject one flit into the books without accounting for it -> loss detected.
    s.injected_flits += 1;
    assert!(matches!(
        InvariantChecker::check_flit_conservation(&s),
        Err(Violation::FlitConservation { .. })
    ));
}

#[test]
fn deadlock_detector_fires_on_a_frozen_network() {
    // Load the network, stop injecting, then feed repeated no-progress
    // snapshots: a stand-in for a hung network with flits stuck in flight.
    let mut sim = build_sim(4, 4, cfg(9), 0.3);
    sim.set_injecting(true);
    for _ in 0..500 {
        sim.tick();
    }
    sim.set_injecting(false);
    assert!(sim.in_flight() > 0, "need flits in flight to model a hang");

    let frozen = sample(&sim);
    let mut deadlock = DeadlockDetector::new(300);
    let mut fired_at = None;
    for k in 0..10u64 {
        let mut s = frozen.clone();
        s.cycle = 1000 + k * 100; // clock advances, delivery/movement frozen
        for ev in deadlock.observe(&s) {
            fired_at.get_or_insert(ev.cycle);
        }
    }
    assert!(
        fired_at.is_some(),
        "deadlock detector should fire on a frozen network"
    );
}
