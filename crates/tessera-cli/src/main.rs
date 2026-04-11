//! `tessera` — headless Phase 0 runner. Sweeps offered load over a 2D mesh and
//! prints (and optionally writes) a latency-throughput curve.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

use tessera_cli::{run_point, RoutingKind, RunParams};
use tessera_core::SimConfig;
use tessera_stats::{write_csv, RunPoint};

const USAGE: &str = "\
tessera — Phase 0 NoC latency-throughput runner

USAGE:
    tessera [OPTIONS]

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

fn print_row(p: &RunPoint) {
    println!(
        "{:>12.4}  {:>14.4}  {:>16.4}  {:>14.2}",
        p.offered_pkt_rate, p.offered_flit_rate, p.accepted_flit_rate, p.avg_packet_latency
    );
}

fn parse(args: &[String]) -> Result<Opts, String> {
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

    let cfg = SimConfig {
        buffer_depth: get(&map, "buffer", 4)?,
        num_vcs: get(&map, "vcs", 1)?,
        router_latency: get(&map, "router-latency", 1)?,
        link_latency: get(&map, "link-latency", 1)?,
        credit_delay: get(&map, "credit-delay", 1)?,
        packet_size: get(&map, "packet-size", 4)?,
        seed: get(&map, "seed", 1)?,
    };
    let routing = match map.get("routing") {
        Some(v) => RoutingKind::parse(v).ok_or_else(|| format!("unknown routing '{v}'"))?,
        None => RoutingKind::Xy,
    };
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
