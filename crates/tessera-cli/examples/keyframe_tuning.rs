//! Phase 4 keyframe-interval/compression tuning benchmark (P4-F8, gate N3):
//! reports the on-disk-size vs seek-latency tradeoff across keyframe
//! intervals and compression choices, on a fixed scenario.
//!
//! Run with: `cargo run --example keyframe_tuning -p tessera-cli`
//! (the `tessera-record` dev-dependency already enables the features this
//! needs; see `docs/phases/PHASE_4_PRD.md` for a captured run's numbers).

use std::time::{Duration, Instant};

use tessera_core::{SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_record::{Compression, KeyframeReader, KeyframeWriter, SeekEngine, SimSource};
use tessera_routing::{DimensionOrderXy, FirstAvailable, RoundRobin};
use tessera_traffic::Uniform;

struct UniformXySource {
    cfg: SimConfig,
}

impl SimSource for UniformXySource {
    fn fresh(&self) -> Simulation {
        Simulation::new(
            self.cfg,
            Box::new(Mesh2D::new(8, 8)),
            Box::new(DimensionOrderXy),
            Box::new(FirstAvailable),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(Uniform::new(64, 0.3, self.cfg.packet_size)),
        )
    }
}

fn dir_size(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Writes a full keyframe store (cycle 0 plus every `interval`th cycle) for
/// `source` ticked to `until`, under `compression`.
fn record_keyframes(
    source: &UniformXySource,
    until: u64,
    interval: u64,
    compression: Compression,
    dir: &std::path::Path,
) {
    let writer = KeyframeWriter::with_compression(dir, compression).unwrap();
    let mut sim = source.fresh();
    writer.write(&sim.snapshot()).unwrap();
    while sim.cycle < until {
        sim.tick();
        if sim.cycle.is_multiple_of(interval) {
            writer.write(&sim.snapshot()).unwrap();
        }
    }
}

/// Seeks to a spread of mid-interval cycles (the worst case for a given
/// interval: as far as possible from the nearest keyframe) and reports mean
/// replay-tick count and wall-clock time.
fn probe_seek_cost(
    reader: &KeyframeReader,
    engine: &SeekEngine<'_, UniformXySource>,
    until: u64,
    interval: u64,
) -> (u64, Duration) {
    let step = (until / interval / 6).max(1);
    let targets: Vec<u64> = (1..until / interval)
        .step_by(step as usize)
        .map(|k| k * interval + interval / 2)
        .filter(|&t| t < until)
        .collect();

    let mut total_replay_ticks = 0u64;
    let mut total_wall = Duration::ZERO;
    for &t in &targets {
        let kf = reader.nearest_le(t).unwrap_or(0);
        total_replay_ticks += t - kf;
        let start = Instant::now();
        let _ = engine.seek_to(t).unwrap();
        total_wall += start.elapsed();
    }
    let n = targets.len().max(1) as u32;
    (total_replay_ticks / n as u64, total_wall / n)
}

fn main() {
    let source = UniformXySource {
        cfg: SimConfig {
            buffer_depth: 8,
            num_vcs: 2,
            router_latency: 1,
            link_latency: 1,
            credit_delay: 1,
            packet_size: 4,
            seed: 99,
        },
    };
    let until = 5000u64;
    let intervals = [25u64, 50, 100, 250, 500];
    let compressions = [Compression::None, Compression::Gzip];

    println!(
        "{:>10} {:>10} {:>14} {:>18} {:>18}",
        "interval", "compress", "total_bytes", "avg_replay_ticks", "avg_seek_wall_us"
    );

    for &interval in &intervals {
        for &compression in &compressions {
            let dir = std::env::temp_dir().join(format!(
                "tessera_keyframe_tuning_{interval}_{compression:?}_{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);

            record_keyframes(&source, until, interval, compression, &dir);

            let total_bytes = dir_size(&dir);
            let reader = KeyframeReader::open(&dir).unwrap();
            let engine = SeekEngine::with_keyframes(&source, &reader);
            let (avg_replay_ticks, avg_wall) = probe_seek_cost(&reader, &engine, until, interval);

            println!(
                "{:>10} {:>10?} {:>14} {:>18} {:>18}",
                interval,
                compression,
                total_bytes,
                avg_replay_ticks,
                avg_wall.as_micros()
            );

            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
