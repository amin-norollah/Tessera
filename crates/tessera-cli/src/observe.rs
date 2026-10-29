//! Run observation (OUT-11): the PATh chain and diagnostics driven from a live
//! run, on a fixed sampling interval, over observable state only.
//!
//! Every `interval` cycles an [`Observer`]:
//! - takes each router's activity since the previous sample and feeds it to a
//!   [`TeiLoop`] (native power model + [`RcGrid`] thermal network, no
//!   throttling): per-router power is that interval's dynamic power plus
//!   leakage at the router's current temperature (P3-F12's power/thermal
//!   coupling; the RTM throttle feedback into the datapath is still OUT-4);
//! - streams one [`MetricRow`] per router plus a network-wide row (P3-F14) to
//!   a [`BatchSink`] through a bounded-memory [`StreamingRecorder`] (P3-N2);
//! - builds a [`HealthSample`] and runs the deadlock, livelock and congestion
//!   detectors (P3-F15), plus, in verify mode, the [`InvariantChecker`]
//!   (P3-F16).
//!
//! Only whole intervals are sampled: a trailing partial interval at the end of
//! a run is not reported.
//!
//! **Per-router columns.** The kernel measures latency network-wide only, so
//! a per-router row's `latency` is `NaN`. Its `throughput` is the flits
//! ejected at that router per cycle (crossbar traversals not forwarded onto a
//! link), `bandwidth` the flits it forwarded onto links per cycle, and
//! `utilization` its mean input-buffer occupancy at the sample. The network
//! row's `latency` is the mean over packets measured during the interval
//! (`NaN` if none), `throughput` is in flits/node/cycle, `bandwidth` is total
//! link flits per cycle, `power_w` the sum and `temp_c` the mean over routers.
//! Reductions run in fixed router order (P3-N3).
//!
//! The starvation detector needs per-flow grant observables the kernel does
//! not expose yet, so it is not run here.

use std::fs::File;
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use thiserror::Error;

use tessera_core::{ActivityCounts, Simulation};
use tessera_diag::{
    CongestionDetector, DeadlockDetector, Detector, DiagEvent, HealthSample, InvariantChecker,
    LivelockDetector, Violation,
};
use tessera_params::{ParamDb, ParamError};
use tessera_power::NativePowerModel;
use tessera_stats::stream::{mean, sum, BatchSink, MetricRow, StreamingRecorder};
use tessera_thermal::tei::{NoThrottle, TeiConfig, TeiLoop};
use tessera_thermal::{Celsius, RcGrid};

/// The parameter profile used when none is given: the cited PTM 22 nm DB.
pub const DEFAULT_PARAMS: &str = include_str!("../../../params/ptm_22nm.toml");

/// Rows buffered before a flush to the sink.
const BATCH_ROWS: usize = 4096;

/// Router occupancy at/above which the congestion detector counts a router
/// as saturated.
const CONGESTION_OCCUPANCY: f32 = 0.9;

#[derive(Debug, Error)]
pub enum ObserveError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    // Boxed so every `Result<_, ObserveError>` stays small
    // (clippy::result_large_err).
    #[error("parameter profile {}: {source}", path.display())]
    Params {
        path: PathBuf,
        #[source]
        source: Box<ParamError>,
    },

    #[error(
        "{}: Parquet output needs tessera-cli built with the `parquet` feature; \
         use a .csv path or rebuild with --features parquet",
        .0.display()
    )]
    ParquetUnsupported(PathBuf),

    #[error("{}: {message}", path.display())]
    Parquet { path: PathBuf, message: String },
}

/// Load a parameter profile, or the [`DEFAULT_PARAMS`] when `path` is `None`.
pub fn load_params(path: Option<&Path>) -> Result<ParamDb, ObserveError> {
    match path {
        None => Ok(ParamDb::from_toml(DEFAULT_PARAMS).expect("bundled profile parses")),
        Some(path) => ParamDb::load(path).map_err(|source| ObserveError::Params {
            path: path.to_path_buf(),
            source: Box::new(source),
        }),
    }
}

/// How an [`Observer`] samples a run.
#[derive(Clone, Debug)]
pub struct ObserveCfg {
    /// Cycles between samples (clamped to at least 1).
    pub interval: u64,
    /// Detection window (cycles) of the deadlock/livelock/congestion
    /// detectors. Should span several intervals.
    pub diag_window: u64,
    /// Also run the invariant checks (P3-F16 verify mode).
    pub verify: bool,
    /// Power/thermal parameter profile.
    pub params: ParamDb,
}

/// What an observed run found, besides the rows streamed to the sink.
#[derive(Clone, Debug, Default)]
pub struct ObserveReport {
    /// Samples taken (whole intervals).
    pub samples: u64,
    /// Metric rows streamed to the sink.
    pub rows: u64,
    /// Detector findings, in the order they fired.
    pub events: Vec<DiagEvent>,
    /// Invariant violations (verify mode only).
    pub violations: Vec<Violation>,
    /// Hottest router temperature seen at any sample (°C).
    pub peak_temp_c: Celsius,
    /// Network power averaged over the samples (W).
    pub mean_power_w: f64,
}

impl ObserveReport {
    /// A deadlock, livelock or invariant violation: the run is not healthy.
    /// Congestion alone is a performance finding, not a failure.
    pub fn is_failure(&self) -> bool {
        use tessera_diag::DiagKind::{Deadlock, Livelock};
        !self.violations.is_empty()
            || self
                .events
                .iter()
                .any(|e| matches!(e.kind, Deadlock | Livelock))
    }
}

/// Samples a running [`Simulation`]; call [`Observer::on_tick`] after every
/// tick and [`Observer::finish`] at the end.
pub struct Observer<S: BatchSink<MetricRow>> {
    interval: u64,
    verify: bool,
    params: ParamDb,
    tei: TeiLoop<RcGrid, NoThrottle>,
    recorder: StreamingRecorder<MetricRow, S>,
    deadlock: DeadlockDetector,
    livelock: LivelockDetector,
    congestion: CongestionDetector,
    invariants: InvariantChecker,
    prev_activity: Vec<ActivityCounts>,
    prev_ejected: u64,
    prev_latency_sum: u128,
    prev_measured: u64,
    power_sum: f64,
    report: ObserveReport,
}

impl<S: BatchSink<MetricRow>> Observer<S> {
    /// An observer for a `width`×`height` grid of routers (row-major router
    /// ids, as `Mesh2D`/`Torus2D` number them).
    pub fn new(cfg: ObserveCfg, width: u16, height: u16, sink: S) -> Self {
        let interval = cfg.interval.max(1);
        let n = width as usize * height as usize;
        let grid = RcGrid::from_params(height as usize, width as usize, &cfg.params, true);
        let tei = TeiLoop::new(
            NativePowerModel::default(),
            grid,
            NoThrottle,
            TeiConfig::from_interval(interval, &cfg.params),
        );
        Self {
            interval,
            verify: cfg.verify,
            params: cfg.params,
            tei,
            recorder: StreamingRecorder::new(BATCH_ROWS, sink),
            deadlock: DeadlockDetector::new(cfg.diag_window),
            livelock: LivelockDetector::new(cfg.diag_window),
            congestion: CongestionDetector::new(cfg.diag_window, CONGESTION_OCCUPANCY),
            invariants: InvariantChecker::new(),
            prev_activity: vec![ActivityCounts::default(); n],
            prev_ejected: 0,
            prev_latency_sum: 0,
            prev_measured: 0,
            power_sum: 0.0,
            report: ObserveReport {
                peak_temp_c: f32::NEG_INFINITY,
                ..ObserveReport::default()
            },
        }
    }

    /// Sample `sim` if it just completed a whole interval.
    pub fn on_tick(&mut self, sim: &Simulation) {
        if sim.cycle > 0 && sim.cycle.is_multiple_of(self.interval) {
            self.sample(sim);
        }
    }

    fn sample(&mut self, sim: &Simulation) {
        let cycle = sim.cycle;
        let span = self.interval as f64;
        let nodes = self.prev_activity.len();

        let now = sim.activity();
        assert_eq!(now.len(), nodes, "observer grid must match the topology");
        let deltas: Vec<ActivityCounts> = now
            .iter()
            .zip(&self.prev_activity)
            .map(|(a, b)| a.since(b))
            .collect();
        self.prev_activity.copy_from_slice(now);

        let temps = self.tei.advance(&deltas, &self.params).to_vec();
        let power = self.tei.power_w().to_vec();
        let occupancy = sim.router_occupancy();

        let mut ejected = Vec::with_capacity(nodes);
        let mut forwarded = Vec::with_capacity(nodes);
        for (r, d) in deltas.iter().enumerate() {
            let local = (d.xbar - d.link_flits) as f64 / span;
            let links = d.link_flits as f64 / span;
            ejected.push(local);
            forwarded.push(links);
            self.recorder.push(MetricRow {
                cycle,
                node: Some(r as u32),
                latency: f64::NAN,
                throughput: local,
                bandwidth: links,
                utilization: occupancy[r] as f64,
                power_w: power[r],
                temp_c: temps[r] as f64,
            });
        }

        let m = &sim.metrics;
        let measured = m.measured_packets - self.prev_measured;
        let latency = if measured == 0 {
            f64::NAN
        } else {
            (m.latency_sum - self.prev_latency_sum) as f64 / measured as f64
        };
        let temps_f64: Vec<f64> = temps.iter().map(|&t| t as f64).collect();
        let occupancy_f64: Vec<f64> = occupancy.iter().map(|&o| o as f64).collect();
        let net_power = sum(&power);
        self.recorder.push(MetricRow {
            cycle,
            node: None,
            latency,
            throughput: (m.ejected_flits - self.prev_ejected) as f64 / (span * nodes as f64),
            bandwidth: sum(&forwarded),
            utilization: mean(&occupancy_f64),
            power_w: net_power,
            temp_c: mean(&temps_f64),
        });
        self.prev_ejected = m.ejected_flits;
        self.prev_latency_sum = m.latency_sum;
        self.prev_measured = m.measured_packets;

        let health = HealthSample {
            cycle,
            delivered_flits: m.ejected_flits,
            total_hops: m.total_hops,
            injected_flits: m.injected_flits,
            dropped_flits: 0, // lossless kernel
            in_flight_flits: sim.in_flight(),
            router_occupancy: occupancy,
            flow_pending: Vec::new(),
            flow_granted: Vec::new(),
        };
        let events = &mut self.report.events;
        events.extend(self.deadlock.observe(&health));
        events.extend(self.livelock.observe(&health));
        events.extend(self.congestion.observe(&health));
        if self.verify {
            self.report
                .violations
                .extend(self.invariants.observe(&health));
        }

        self.report.samples += 1;
        self.power_sum += net_power;
        for &t in &temps {
            self.report.peak_temp_c = self.report.peak_temp_c.max(t);
        }
    }

    /// Flush the rows and return the sink plus the run's findings.
    pub fn finish(mut self) -> (S, ObserveReport) {
        self.report.rows = self.recorder.total_rows();
        if self.report.samples > 0 {
            self.report.mean_power_w = self.power_sum / self.report.samples as f64;
        } else {
            self.report.peak_temp_c = self.params.thermal.t_amb_c.value as f32;
        }
        (self.recorder.finish(), self.report)
    }
}

/// Where a run's metric time-series goes: chosen from the output path's
/// extension (`.parquet` → Arrow/Parquet, anything else → CSV), or discarded
/// when only diagnostics are wanted.
pub enum MetricsOut {
    Discard,
    Csv(CsvFileSink),
    #[cfg(feature = "parquet")]
    Parquet {
        path: PathBuf,
        // Boxed: the Arrow writer dwarfs the other variants
        // (clippy::large_enum_variant).
        sink: Box<tessera_stats::parquet_export::ParquetSink>,
    },
}

impl MetricsOut {
    /// Create the output file at `path`.
    pub fn create(path: &Path) -> Result<Self, ObserveError> {
        let parquet = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("parquet"));
        if parquet {
            Self::create_parquet(path)
        } else {
            CsvFileSink::create(path).map(MetricsOut::Csv)
        }
    }

    #[cfg(feature = "parquet")]
    fn create_parquet(path: &Path) -> Result<Self, ObserveError> {
        let sink =
            tessera_stats::parquet_export::ParquetSink::create(path, BATCH_ROWS).map_err(|e| {
                ObserveError::Parquet {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                }
            })?;
        Ok(MetricsOut::Parquet {
            path: path.to_path_buf(),
            sink: Box::new(sink),
        })
    }

    #[cfg(not(feature = "parquet"))]
    fn create_parquet(path: &Path) -> Result<Self, ObserveError> {
        Err(ObserveError::ParquetUnsupported(path.to_path_buf()))
    }

    /// Surface the first write/close error, if any (call after
    /// [`Observer::finish`]).
    pub fn into_result(self) -> Result<(), ObserveError> {
        match self {
            MetricsOut::Discard => Ok(()),
            MetricsOut::Csv(sink) => sink.into_result(),
            #[cfg(feature = "parquet")]
            MetricsOut::Parquet { path, sink } => {
                sink.into_result().map_err(|e| ObserveError::Parquet {
                    path,
                    message: e.to_string(),
                })
            }
        }
    }
}

impl BatchSink<MetricRow> for MetricsOut {
    fn write_batch(&mut self, rows: &[MetricRow]) {
        match self {
            MetricsOut::Discard => {}
            MetricsOut::Csv(sink) => sink.write_batch(rows),
            #[cfg(feature = "parquet")]
            MetricsOut::Parquet { sink, .. } => sink.write_batch(rows),
        }
    }

    fn finish(&mut self) {
        match self {
            MetricsOut::Discard => {}
            MetricsOut::Csv(sink) => sink.finish(),
            #[cfg(feature = "parquet")]
            MetricsOut::Parquet { sink, .. } => sink.finish(),
        }
    }
}

/// A [`BatchSink`] streaming [`MetricRow`]s to a CSV file (header first),
/// keeping nothing in memory beyond the write buffer. Like
/// `tessera_stats::parquet_export::ParquetSink`, the first I/O error is
/// latched and stops further writes; surface it with
/// [`CsvFileSink::into_result`].
pub struct CsvFileSink {
    path: PathBuf,
    out: Option<BufWriter<File>>,
    error: Option<io::Error>,
}

impl CsvFileSink {
    pub fn create(path: &Path) -> Result<Self, ObserveError> {
        let io_err = |source| ObserveError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut out = BufWriter::new(File::create(path).map_err(io_err)?);
        writeln!(out, "{}", MetricRow::COLUMNS.join(",")).map_err(io_err)?;
        Ok(Self {
            path: path.to_path_buf(),
            out: Some(out),
            error: None,
        })
    }

    pub fn into_result(self) -> Result<(), ObserveError> {
        match self.error {
            Some(source) => Err(ObserveError::Io {
                path: self.path,
                source,
            }),
            None => Ok(()),
        }
    }
}

impl BatchSink<MetricRow> for CsvFileSink {
    fn write_batch(&mut self, rows: &[MetricRow]) {
        let (Some(out), None) = (self.out.as_mut(), &self.error) else {
            return;
        };
        for row in rows {
            if let Err(e) = writeln!(out, "{}", row.to_columns().join(",")) {
                self.error = Some(e);
                return;
            }
        }
    }

    fn finish(&mut self) {
        if let Some(mut out) = self.out.take() {
            if let Err(e) = out.flush() {
                self.error.get_or_insert(e);
            }
        }
    }
}
