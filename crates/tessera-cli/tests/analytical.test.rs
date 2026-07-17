//! Analytical / theoretical validation, substituting for G1.1's literal
//! BookSim/Noxim differential comparison (neither tool is available in this
//! environment: see `docs/phases/PHASE_1_PRD.md` for the documented
//! rationale). Each check compares simulated behavior against a closed-form
//! bound or a theory-predicted ordering, not a fitted/eyeballed number.

use tessera_core::{RoutingAlgorithm, SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_routing::{DimensionOrderXy, FirstAvailable, OddEven, RoundRobin};
use tessera_traffic::Transpose;

fn accepted_flit_rate(routing: Box<dyn RoutingAlgorithm>, rate: f64) -> f64 {
    let cfg = SimConfig {
        buffer_depth: 4,
        num_vcs: 2,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 11,
    };
    let (width, height) = (8u16, 8u16);
    let nodes = width as u32 * height as u32;
    let mut sim = Simulation::new(
        cfg,
        Box::new(Mesh2D::new(width, height)),
        routing,
        Box::new(FirstAvailable),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(RoundRobin::new()),
        Box::new(Transpose::new(width as u32, rate, cfg.packet_size)),
    );
    sim.measure_start = 2000;
    sim.measure_end = 8000;
    sim.set_injecting(true);
    for _ in 0..8000 {
        sim.tick();
    }
    let measure_cycles = (sim.measure_end - sim.measure_start) as f64;
    sim.metrics.ejected_flits_in_window as f64 / (nodes as f64 * measure_cycles)
}

/// Bisection-bandwidth upper bound (G1.1 substitute, part 1): on a `width`-
/// wide mesh, a vertical cut has `height` links carrying traffic each way.
/// Under uniform random destinations, on average half of all accepted
/// traffic crosses any given bisection, split ~evenly between the two
/// directions, so one direction's flow can't exceed `height` flits/cycle:
/// `0.25 * accepted_flit_rate * nodes <= height`, i.e.
/// `accepted_flit_rate <= 4 / width`. This is a hard physical ceiling
/// (capacity), not a fitted constant: actual saturation throughput sits
/// well below it because DOR doesn't perfectly balance load across the cut.
#[test]
fn saturation_throughput_stays_within_the_bisection_bandwidth_bound() {
    let cfg = SimConfig {
        buffer_depth: 4,
        num_vcs: 1,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed: 3,
    };
    let p = tessera_cli::RunParams {
        width: 8,
        height: 8,
        cfg,
        routing: "xy".to_string(),
        warmup: 2000,
        measure: 8000,
        drain_cap: 200_000,
    };
    let point = tessera_cli::run_point(&p, 0.5); // deep past saturation
    let bound = 4.0 / p.width as f64;
    assert!(
        point.accepted_flit_rate <= bound,
        "accepted {} exceeds the bisection-bandwidth ceiling {bound}: physically impossible, so this would mean a metrics bug",
        point.accepted_flit_rate
    );
    assert!(
        point.accepted_flit_rate >= bound * 0.3,
        "accepted {} is suspiciously far below the {bound} ceiling: possible throughput regression",
        point.accepted_flit_rate
    );
}

/// G1.1 substitute, part 2: odd-even's genuine two-direction adaptivity
/// should beat plain dimension-order routing under an adversarial
/// (non-uniform) pattern: this is the theoretical payoff adaptive routing
/// is supposed to deliver, per `docs/RESEARCH_NOTES.md`'s routing/selection
/// sources. Transpose traffic (node `(x,y)` -> `(y,x)`) is the standard
/// adversarial pattern for exactly this comparison (INITIAL_PRD §20.8,
/// C7 traffic-pattern methodology).
#[test]
fn adaptive_routing_beats_dor_under_transpose_traffic() {
    let rate = 0.1;
    let xy = accepted_flit_rate(Box::new(DimensionOrderXy), rate);
    let odd_even = accepted_flit_rate(Box::new(OddEven), rate);
    assert!(
        odd_even > xy * 1.05,
        "odd-even ({odd_even}) should measurably beat XY ({xy}) under transpose traffic"
    );
}
