//! Run observation (OUT-11, P3-F14/F15/F16): an `Observer` attached to a live
//! configured run streams a per-router + network power/thermal/traffic
//! time-series whose columns reconcile with the kernel's own counters, runs
//! the diagnostics, and never perturbs the run it watches. The `tessera`
//! binary exposes it as `--metrics`/`--verify`.

use std::path::PathBuf;
use std::process::Command;

use tessera_cli::observe::{load_params, MetricsOut, ObserveCfg, ObserveReport, Observer};
use tessera_cli::{build_from_config, run_configured, run_configured_with, ConfiguredRun};
use tessera_config::{SimConfigFile, TopologyCfg, TrafficCfg, TrafficKind};
use tessera_core::{RouterId, Simulation};
use tessera_power::NativePowerModel;
use tessera_stats::stream::{MetricRow, VecSink};

const W: u16 = 4;
const H: u16 = 4;
const NODES: usize = (W as usize) * (H as usize);
const INTERVAL: u64 = 200;

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tessera_cli_observe_test_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config(rate: f64) -> SimConfigFile {
    SimConfigFile {
        topology: TopologyCfg::Mesh2D {
            width: W,
            height: H,
        },
        traffic: TrafficCfg {
            kind: TrafficKind::Uniform {
                injection_rate: rate,
            },
            ..TrafficCfg::default()
        },
        warmup: 400,
        measure: 2000,
        ..SimConfigFile::default()
    }
}

fn build(rate: f64) -> ConfiguredRun {
    build_from_config(&config(rate), std::path::Path::new(".")).unwrap()
}

fn observe_cfg(verify: bool) -> ObserveCfg {
    ObserveCfg {
        interval: INTERVAL,
        diag_window: 2000,
        verify,
        params: load_params(None).unwrap(),
    }
}

/// Drive `run` injecting for exactly `intervals` whole intervals (no drain),
/// so the last sample lands on the final cycle and the run's cumulative
/// counters are directly comparable with the summed rows.
fn drive(mut run: ConfiguredRun, intervals: u64) -> (Simulation, Vec<MetricRow>, ObserveReport) {
    let mut obs = Observer::new(observe_cfg(true), run.width, run.height, VecSink::default());
    run.sim.set_injecting(true);
    for _ in 0..intervals * INTERVAL {
        run.sim.tick();
        obs.on_tick(&run.sim);
    }
    let (sink, report) = obs.finish();
    (run.sim, sink.rows, report)
}

#[test]
fn rows_cover_every_router_and_reconcile_with_kernel_counters() {
    let intervals = 10;
    let (sim, rows, report) = drive(build(0.15), intervals);

    assert_eq!(report.samples, intervals);
    assert_eq!(rows.len() as u64, intervals * (NODES as u64 + 1));
    assert_eq!(report.rows, rows.len() as u64);

    let span = INTERVAL as f64;
    let (mut ejected, mut hops) = (0.0, 0.0);
    for (k, sample) in rows.chunks(NODES + 1).enumerate() {
        let (nodes, net) = sample.split_at(NODES);
        let net = &net[0];
        let cycle = (k as u64 + 1) * INTERVAL;
        assert!(sample.iter().all(|r| r.cycle == cycle));
        for (r, row) in nodes.iter().enumerate() {
            assert_eq!(row.node, Some(r as u32), "router rows in id order");
            assert!(row.latency.is_nan(), "no per-router latency observable");
            assert!(row.power_w > 0.0 && row.power_w.is_finite());
            assert!((0.0..=1.0).contains(&row.utilization));
        }
        assert_eq!(net.node, None);

        // Network reductions are the fixed-order folds of the router rows.
        let node_power = nodes.iter().fold(0.0, |acc, r| acc + r.power_w);
        assert_eq!(net.power_w, node_power);
        let node_bw = nodes.iter().fold(0.0, |acc, r| acc + r.bandwidth);
        assert_eq!(net.bandwidth, node_bw);
        let node_tp = nodes.iter().fold(0.0, |acc, r| acc + r.throughput);
        assert!((net.throughput * NODES as f64 - node_tp).abs() < 1e-12);

        ejected += node_tp * span;
        hops += node_bw * span;
    }
    // Per-router ejections (xbar not forwarded onto a link) and link flits,
    // summed over every interval, are exactly the kernel's totals.
    assert_eq!(ejected.round() as u64, sim.metrics.ejected_flits);
    assert_eq!(hops.round() as u64, sim.metrics.total_hops);
    assert!(sim.metrics.ejected_flits > 0, "traffic actually flowed");
}

#[test]
fn reported_energy_is_dynamic_energy_plus_leakage() {
    let intervals = 8;
    let (sim, rows, report) = drive(build(0.2), intervals);

    let p = load_params(None).unwrap();
    let model = NativePowerModel::default();
    let dynamic: f64 = sim
        .activity()
        .iter()
        .enumerate()
        .map(|(r, c)| model.energy_from_counts(c, RouterId(r as u32), &p).total())
        .sum();

    let dt = INTERVAL as f64 / p.electrical.frequency.value;
    let reported: f64 = rows
        .iter()
        .filter(|r| r.node.is_none())
        .map(|r| r.power_w * dt)
        .sum();
    // Leakage is strictly positive, so the reported energy exceeds the
    // dynamic energy of the very same activity, by at least one tile's
    // ambient leakage over the run.
    let min_leak = model.leakage_current(p.thermal.t_amb_c.value, &p)
        * p.electrical.voltage.value
        * dt
        * intervals as f64;
    assert!(
        reported >= dynamic + min_leak * (1.0 - 1e-9),
        "reported {reported:e} J vs dynamic {dynamic:e} J + leakage"
    );

    let amb = p.thermal.t_amb_c.value as f32;
    assert!(report.peak_temp_c >= amb && report.peak_temp_c.is_finite());
    assert!(report.mean_power_w > 0.0);
}

#[test]
fn observation_is_deterministic() {
    let render = |rows: Vec<MetricRow>| -> Vec<[String; 8]> {
        rows.iter().map(MetricRow::to_columns).collect()
    };
    let (_, a, _) = drive(build(0.2), 6);
    let (_, b, _) = drive(build(0.2), 6);
    assert_eq!(render(a), render(b));
}

#[test]
fn observing_does_not_perturb_the_run() {
    let plain = run_configured(build(0.2), 100_000);

    let run = build(0.2);
    let mut obs = Observer::new(observe_cfg(true), run.width, run.height, VecSink::default());
    let observed = run_configured_with(run, 100_000, |sim| obs.on_tick(sim));
    let (_, report) = obs.finish();

    assert_eq!(plain.injected_packets, observed.injected_packets);
    assert_eq!(plain.ejected_packets, observed.ejected_packets);
    assert_eq!(plain.avg_packet_latency, observed.avg_packet_latency);
    assert_eq!(plain.accepted_flit_rate, observed.accepted_flit_rate);
    assert!(report.samples > 0);
}

#[test]
fn healthy_run_passes_verify_mode() {
    let run = build(0.15);
    let mut obs = Observer::new(observe_cfg(true), run.width, run.height, VecSink::default());
    run_configured_with(run, 100_000, |sim| obs.on_tick(sim));
    let (_, report) = obs.finish();

    assert!(report.events.is_empty(), "{:?}", report.events);
    assert!(report.violations.is_empty(), "{:?}", report.violations);
    assert!(!report.is_failure());
}

#[cfg(not(feature = "parquet"))]
#[test]
fn parquet_output_without_the_feature_is_a_clear_error() {
    let dir = scratch_dir("no_parquet");
    let err = MetricsOut::create(&dir.join("m.parquet")).err().unwrap();
    assert!(err.to_string().contains("parquet"), "{err}");
}

#[cfg(feature = "parquet")]
#[test]
fn parquet_output_reads_back() {
    use parquet::file::reader::{FileReader, SerializedFileReader};

    let dir = scratch_dir("parquet");
    let path = dir.join("m.parquet");
    let run = build(0.15);
    let out = MetricsOut::create(&path).unwrap();
    let mut obs = Observer::new(observe_cfg(false), run.width, run.height, out);
    run_configured_with(run, 100_000, |sim| obs.on_tick(sim));
    let (out, report) = obs.finish();
    out.into_result().unwrap();

    let reader = SerializedFileReader::new(std::fs::File::open(&path).unwrap()).unwrap();
    let rows = reader.metadata().file_metadata().num_rows();
    assert_eq!(rows as u64, report.rows);
}

#[test]
fn cli_writes_metrics_csv_and_passes_verify() {
    let dir = scratch_dir("cli");
    let config = dir.join("run.toml");
    std::fs::write(&config, toml::to_string(&config_for_cli()).unwrap()).unwrap();
    let metrics = dir.join("metrics.csv");

    let out = Command::new(env!("CARGO_BIN_EXE_tessera"))
        .arg("--config")
        .arg(&config)
        .arg("--metrics")
        .arg(&metrics)
        .args(["--interval", "100", "--verify"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("verify: PASS"), "{stdout}");

    let csv = std::fs::read_to_string(&metrics).unwrap();
    let mut lines = csv.lines();
    assert_eq!(lines.next(), Some(MetricRow::COLUMNS.join(",").as_str()));
    let body: Vec<&str> = lines.collect();
    // Whole intervals of the injecting phase alone: 2400 / 100 = 24 samples,
    // plus however many the drain adds.
    assert!(body.len() >= 24 * (NODES + 1), "{} rows", body.len());
    assert_eq!(body.len() % (NODES + 1), 0, "whole samples only");
    assert_eq!(
        body.iter().filter(|l| l.contains(",net,")).count() * (NODES + 1),
        body.len()
    );
}

#[test]
fn cli_rejects_a_missing_params_profile() {
    let dir = scratch_dir("cli_params");
    let config = dir.join("run.toml");
    std::fs::write(&config, toml::to_string(&config_for_cli()).unwrap()).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_tessera"))
        .arg("--config")
        .arg(&config)
        .args(["--verify", "--params"])
        .arg(dir.join("missing.toml"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing.toml"), "{stderr}");
}

fn config_for_cli() -> SimConfigFile {
    config(0.1)
}
