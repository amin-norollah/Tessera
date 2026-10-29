//! `tessera`: headless runner. Sweeps offered load over a 2D mesh and prints
//! (and optionally writes) a latency-throughput curve, or runs one point from
//! a full TOML config (`--config`, every component choice honored).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tessera_cli::observe::{load_params, MetricsOut, ObserveCfg, Observer};
use tessera_cli::{
    build_from_path, run_configured, run_configured_with, run_point, ConfiguredRun, RunParams,
};
use tessera_core::SimConfig;
use tessera_plugin::builtin_registry;
use tessera_stats::{write_csv, RunPoint};

const USAGE: &str = "\
tessera: Phase 0 NoC latency-throughput runner

USAGE:
    tessera [OPTIONS]
    tessera --config PATH [--out PATH] [--drain-cap N]

CONFIG FILE:
    --config PATH        run one point from a TOML config (tessera-config
                         SimConfigFile schema: topology, router, allocators,
                         selection, traffic incl. table/netrace/synfull).
                         Relative traffic paths resolve against the config's
                         directory. Excludes the flags below except OUTPUT
                         and --drain-cap.

OBSERVATION (with --config):
    --metrics PATH       write a per-router + network power/thermal/traffic
                         time-series; .parquet needs the `parquet` feature,
                         any other extension writes CSV
    --params PATH        power/thermal parameter profile [default: bundled
                         params/ptm_22nm.toml]
    --interval N         sampling interval (cycles)    [default: 1000]
    --diag-window N      deadlock/livelock/congestion window (cycles)
                                                       [default: 10000]
    --verify             also check invariants (flit conservation, monotonic
                         counters); exit non-zero on a violation, deadlock or
                         livelock

TOPOLOGY / ROUTER:
    --width N            mesh width            [default: 8]
    --height N           mesh height           [default: 8]
    --buffer N           buffer depth (flits)  [default: 4]
    --vcs N              virtual channels/port [default: 1]
    --packet-size N      flits per packet      [default: 4]
    --router-latency N   cycles per router     [default: 1]
    --link-latency N     cycles per link       [default: 1]
    --credit-delay N     credit return delay   [default: 1]
    --routing NAME       xy | xyz | west-first | north-last | negative-first  [default: xy]
    --seed N             master RNG seed       [default: 1]

WORKLOAD / HARNESS:
    --rate F             single offered load (packets/node/cycle); disables sweep
    --rate-min F         sweep start           [default: 0.02]
    --rate-max F         sweep end (inclusive) [default: 0.50]
    --rate-step F        sweep step            [default: 0.02]
    --warmup N           warm-up cycles        [default: 2000]
    --measure N          measurement cycles    [default: 10000]
    --drain-cap N        max drain cycles      [default: 200000]

OUTPUT:
    --out PATH           write the curve as CSV
    -h, --help           print this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--config") {
        return run_config_file(&args);
    }
    match parse(&args) {
        Ok(opts) => {
            run(opts);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

struct Opts {
    params: RunParams,
    rates: Vec<f64>,
    out: Option<PathBuf>,
}

fn run(opts: Opts) {
    println!(
        "mesh {}x{}  buffer={}  packet={}  t_r={}  t_w={}  seed={}",
        opts.params.width,
        opts.params.height,
        opts.params.cfg.buffer_depth,
        opts.params.cfg.packet_size,
        opts.params.cfg.router_latency,
        opts.params.cfg.link_latency,
        opts.params.cfg.seed,
    );
    println!(
        "{:>12}  {:>14}  {:>16}  {:>14}",
        "offered_pkt", "offered_flit", "accepted_flit", "avg_latency"
    );

    let mut points = Vec::new();
    for &rate in &opts.rates {
        let p = run_point(&opts.params, rate);
        print_row(&p);
        points.push(p);
    }

    if let Some(path) = &opts.out {
        match write_csv(path, &points) {
            Ok(()) => println!("\nwrote {} points to {}", points.len(), path.display()),
            Err(e) => eprintln!("failed to write {}: {e}", path.display()),
        }
    }
}

/// Keys accepted alongside `--config`: everything else is the config's job.
const CONFIG_COMPATIBLE: [&str; 7] = [
    "config",
    "out",
    "drain-cap",
    "metrics",
    "params",
    "interval",
    "diag-window",
];

fn run_config_file(args: &[String]) -> ExitCode {
    // `--verify` is the one valueless flag.
    let verify = args.iter().any(|a| a == "--verify");
    let args: Vec<String> = args.iter().filter(|a| *a != "--verify").cloned().collect();
    let result = parse_pairs(&args).and_then(|map| {
        if let Some(bad) = map
            .keys()
            .find(|k| !CONFIG_COMPATIBLE.contains(&k.as_str()))
        {
            return Err(format!(
                "--{bad} cannot be combined with --config; set it in the config file"
            ));
        }
        let drain_cap: u64 = get(&map, "drain-cap", 200_000)?;
        Ok((map, drain_cap))
    });
    let (map, drain_cap) = match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "error: {e}

{USAGE}"
            );
            return ExitCode::FAILURE;
        }
    };
    let path = PathBuf::from(&map["config"]);
    let run = match build_from_path(&path) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("config {}", path.display());
    if let Some(stamp) = &run.provenance {
        print!("{stamp}");
    }
    println!(
        "{:>12}  {:>14}  {:>16}  {:>14}",
        "offered_pkt", "offered_flit", "accepted_flit", "avg_latency"
    );
    let observing = verify || map.contains_key("metrics");
    let (point, healthy) = if observing {
        match observe_run(run, drain_cap, &map, verify) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        let point = run_configured(run, drain_cap);
        print_row(&point);
        (point, true)
    };

    if let Some(out) = map.get("out").map(PathBuf::from) {
        if let Err(e) = write_csv(&out, std::slice::from_ref(&point)) {
            eprintln!("failed to write {}: {e}", out.display());
            return ExitCode::FAILURE;
        }
        println!(
            "
wrote 1 point to {}",
            out.display()
        );
    }
    if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Run with an [`Observer`] attached and print its findings. Returns the
/// point and whether the run passed (always true without `--verify`).
fn observe_run(
    run: ConfiguredRun,
    drain_cap: u64,
    map: &HashMap<String, String>,
    verify: bool,
) -> Result<(RunPoint, bool), String> {
    let cfg = ObserveCfg {
        interval: get(map, "interval", 1000)?,
        diag_window: get(map, "diag-window", 10_000)?,
        verify,
        params: load_params(map.get("params").map(Path::new)).map_err(|e| e.to_string())?,
    };
    let metrics_path = map.get("metrics").map(PathBuf::from);
    let out = match &metrics_path {
        Some(path) => MetricsOut::create(path).map_err(|e| e.to_string())?,
        None => MetricsOut::Discard,
    };

    let mut observer = Observer::new(cfg, run.width, run.height, out);
    let point = run_configured_with(run, drain_cap, |sim| observer.on_tick(sim));
    print_row(&point);
    let (out, report) = observer.finish();
    out.into_result().map_err(|e| e.to_string())?;

    println!(
        "\nobserved {} samples: mean network power {:.4e} W, peak temperature {:.3} C",
        report.samples, report.mean_power_w, report.peak_temp_c
    );
    if let Some(path) = &metrics_path {
        println!("wrote {} metric rows to {}", report.rows, path.display());
    }
    for e in &report.events {
        println!(
            "diag: {:?} at {:?}, cycle {}: {}",
            e.kind, e.location, e.cycle, e.detail
        );
    }
    for v in &report.violations {
        println!("verify: {v:?}");
    }
    let healthy = !(verify && report.is_failure());
    if verify {
        println!("verify: {}", if healthy { "PASS" } else { "FAIL" });
    }
    Ok((point, healthy))
}

fn print_row(p: &RunPoint) {
    println!(
        "{:>12.4}  {:>14.4}  {:>16.4}  {:>14.2}",
        p.offered_pkt_rate, p.offered_flit_rate, p.accepted_flit_rate, p.avg_packet_latency
    );
}

fn parse_pairs(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let key = &args[i];
        let name = key
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument '{key}'"))?;
        let val = args
            .get(i + 1)
            .ok_or_else(|| format!("missing value for --{name}"))?;
        map.insert(name.to_string(), val.clone());
        i += 2;
    }
    Ok(map)
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let map = parse_pairs(args)?;

    let cfg = SimConfig {
        buffer_depth: get(&map, "buffer", 4)?,
        num_vcs: get(&map, "vcs", 1)?,
        router_latency: get(&map, "router-latency", 1)?,
        link_latency: get(&map, "link-latency", 1)?,
        credit_delay: get(&map, "credit-delay", 1)?,
        packet_size: get(&map, "packet-size", 4)?,
        seed: get(&map, "seed", 1)?,
    };
    let routing = map
        .get("routing")
        .cloned()
        .unwrap_or_else(|| "xy".to_string());
    if !builtin_registry().has_routing(&routing) {
        return Err(format!(
            "unknown routing '{routing}' (known: {})",
            builtin_registry().routing_names().join(", ")
        ));
    }
    let params = RunParams {
        width: get(&map, "width", 8)?,
        height: get(&map, "height", 8)?,
        cfg,
        routing,
        warmup: get(&map, "warmup", 2000)?,
        measure: get(&map, "measure", 10000)?,
        drain_cap: get(&map, "drain-cap", 200_000)?,
    };

    let rates = if let Some(v) = map.get("rate") {
        vec![v.parse::<f64>().map_err(|_| "invalid --rate".to_string())?]
    } else {
        let min: f64 = get(&map, "rate-min", 0.02)?;
        let max: f64 = get(&map, "rate-max", 0.50)?;
        let step: f64 = get(&map, "rate-step", 0.02)?;
        sweep(min, max, step)?
    };

    let out = map.get("out").map(PathBuf::from);
    Ok(Opts { params, rates, out })
}

fn sweep(min: f64, max: f64, step: f64) -> Result<Vec<f64>, String> {
    if step <= 0.0 || max < min {
        return Err("invalid sweep range".into());
    }
    let mut rates = Vec::new();
    let mut r = min;
    while r <= max + 1e-9 {
        rates.push((r * 1e6).round() / 1e6);
        r += step;
    }
    Ok(rates)
}

fn get<T: std::str::FromStr>(
    map: &HashMap<String, String>,
    key: &str,
    default: T,
) -> Result<T, String> {
    match map.get(key) {
        Some(v) => v
            .parse::<T>()
            .map_err(|_| format!("invalid value for --{key}")),
        None => Ok(default),
    }
}
