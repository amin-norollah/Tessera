//! Phase 4 Milestone C, exercised over real `Simulation`s: DuckDB SQL over a
//! recorded run's event Parquet store returns aggregates cross-checked
//! against an independently-collected in-memory event trace (P4-F5, gate
//! G4.3 "query correctness"), and comparing two runs that differ in one
//! parameter (injection rate) produces a correct, readable diff (P4-F6, gate
//! G4.3 "run comparison").

use tessera_core::{SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_record::{
    compare_summary, compare_timeseries, run_collecting_events, Query, Recorder, SimSource,
};
use tessera_routing::{DimensionOrderXy, FirstAvailable, RoundRobin};
use tessera_traffic::Uniform;

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

struct UniformXySource {
    cfg: SimConfig,
    rate: f64,
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
            Box::new(Uniform::new(16, self.rate, self.cfg.packet_size)),
        )
    }
}

fn scratch_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tessera_cli_query_and_compare_test_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    path
}

fn record_run(source: &UniformXySource, until: u64, event_path: &std::path::Path) {
    let mut sim = source.fresh();
    let mut recorder =
        Recorder::create(event_path.with_extension("keyframes"), 0, event_path, 256).unwrap();
    recorder.start(&mut sim).unwrap();
    while sim.cycle < until {
        sim.tick();
        recorder.observe(&mut sim).unwrap();
    }
    recorder.finish().unwrap();
}

#[test]
fn duckdb_delivered_count_matches_an_independently_collected_trace() {
    let source = UniformXySource {
        cfg: mesh_cfg(5),
        rate: 0.3,
    };
    let until = 300;
    let event_path = scratch_path("query_correctness.parquet");
    record_run(&source, until, &event_path);

    // The oracle: an independent from-seed run, collected in-memory
    // (Milestone A's helper) — not read through DuckDB at all.
    let mut oracle = source.fresh();
    let oracle_events = run_collecting_events(&mut oracle, until);
    let oracle_delivered = oracle_events
        .iter()
        .filter(|e| matches!(e, tessera_core::Event::Delivered { .. }))
        .count();

    let query = Query::open(&event_path).unwrap();
    let duckdb_delivered = query
        .sql("SELECT COUNT(*) FROM events WHERE kind = 'delivered'")
        .unwrap()
        .scalar_f64()
        .unwrap();
    assert_eq!(duckdb_delivered as usize, oracle_delivered);
    assert!(oracle_delivered > 0, "test scenario delivered no packets");

    let avg_latency = query
        .sql("SELECT AVG(latency) FROM events WHERE kind = 'delivered'")
        .unwrap()
        .scalar_f64()
        .unwrap();
    let manual_avg_latency: f64 = {
        let latencies: Vec<u64> = oracle_events
            .iter()
            .filter_map(|e| match e {
                tessera_core::Event::Delivered { latency, .. } => Some(*latency),
                _ => None,
            })
            .collect();
        latencies.iter().sum::<u64>() as f64 / latencies.len() as f64
    };
    assert_eq!(avg_latency, manual_avg_latency);
}

#[test]
fn comparing_two_runs_at_different_injection_rates_produces_a_correct_diff() {
    let until = 400;
    let low = UniformXySource {
        cfg: mesh_cfg(7),
        rate: 0.1,
    };
    let high = UniformXySource {
        cfg: mesh_cfg(7),
        rate: 0.5,
    };

    let low_path = scratch_path("compare_low.parquet");
    let high_path = scratch_path("compare_high.parquet");
    record_run(&low, until, &low_path);
    record_run(&high, until, &high_path);

    let q_low = Query::open(&low_path).unwrap();
    let q_high = Query::open(&high_path).unwrap();

    let summary = compare_summary(
        &q_low,
        &q_high,
        &[
            (
                "injected_count",
                "SELECT COUNT(*) FROM events WHERE kind = 'injected'",
            ),
            (
                "avg_latency",
                "SELECT AVG(latency) FROM events WHERE kind = 'delivered'",
            ),
        ],
    )
    .unwrap();

    let injected = &summary[0];
    assert_eq!(injected.metric, "injected_count");
    assert!(
        injected.b > injected.a,
        "the higher injection-rate run should inject more packets: {} vs {}",
        injected.a,
        injected.b
    );
    assert_eq!(injected.delta, injected.b - injected.a);

    let latency = &summary[1];
    assert!(
        latency.b >= latency.a,
        "higher offered load should not produce lower average latency: {} vs {}",
        latency.a,
        latency.b
    );

    // Aligned time series: bucket by 50-cycle windows and diff average
    // latency per bucket.
    let time_series = compare_timeseries(
        &q_low,
        &q_high,
        "SELECT CAST(cycle / 50 AS BIGINT) AS bucket, AVG(latency) FROM events \
         WHERE kind = 'delivered' GROUP BY bucket ORDER BY bucket",
    )
    .unwrap();
    assert!(!time_series.is_empty());
    for point in &time_series {
        assert_eq!(point.delta, point.b - point.a);
    }
}
