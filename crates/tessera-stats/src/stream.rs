//! Streaming, bounded-memory metric time-series (P3-F14, P3-N2).
//!
//! PAT-Noxim accumulates every per-cycle sample in an ever-growing
//! `vector<double>`; over a long run that is unbounded memory. Tessera instead
//! **streams** fixed-size batches of metric rows to a [`BatchSink`] and keeps
//! only one batch in RAM at a time, so steady-state memory is `O(batch_size)`
//! regardless of run length (P3-N2).
//!
//! The sink is pluggable: an in-memory [`VecSink`] for tests, a [`CsvSink`]
//! here, and an Arrow/Parquet writer as a drop-in for the columnar store
//! (Phase 3 step, the remaining half of P3-F14). Column reductions
//! ([`mean`], [`sum`]) run in a **fixed left-to-right associative order** so a
//! reduction is identical regardless of thread count (P3-F14 determinism,
//! P3-N3).

use std::fmt::Write as _;

/// One interval's metrics for one scope. `node = None` is the network-wide
/// aggregate; `node = Some(id)` is a per-router row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MetricRow {
    pub cycle: u64,
    pub node: Option<u32>,
    /// Mean packet latency over the interval (cycles).
    pub latency: f64,
    /// Accepted throughput (flits/node/cycle).
    pub throughput: f64,
    /// Bandwidth (flits/cycle).
    pub bandwidth: f64,
    /// Link/buffer utilization in `0.0..=1.0`.
    pub utilization: f64,
    /// Power draw (W).
    pub power_w: f64,
    /// Temperature (°C).
    pub temp_c: f64,
}

impl MetricRow {
    /// The column names, in the order [`Self::to_columns`] emits them.
    pub const COLUMNS: [&'static str; 8] = [
        "cycle",
        "node",
        "latency",
        "throughput",
        "bandwidth",
        "utilization",
        "power_w",
        "temp_c",
    ];

    /// The row as stringly-typed columns, matching [`Self::COLUMNS`]. `node`
    /// renders as `"net"` for the network-wide aggregate.
    pub fn to_columns(&self) -> [String; 8] {
        [
            self.cycle.to_string(),
            self.node
                .map_or_else(|| "net".to_string(), |n| n.to_string()),
            format!("{:.6}", self.latency),
            format!("{:.6}", self.throughput),
            format!("{:.6}", self.bandwidth),
            format!("{:.6}", self.utilization),
            format!("{:.6e}", self.power_w),
            format!("{:.3}", self.temp_c),
        ]
    }
}

/// A destination for streamed metric batches. Implementations must consume each
/// batch fully; the recorder guarantees rows arrive in push order and never
/// re-sends a batch.
pub trait BatchSink {
    /// Write one batch of rows (never empty). May be called many times.
    fn write_batch(&mut self, rows: &[MetricRow]);
    /// Called once at [`StreamingRecorder::finish`]; default no-op.
    fn finish(&mut self) {}
}

/// Buffers metric rows and flushes them to a [`BatchSink`] in fixed-size
/// batches, bounding in-RAM footprint to `capacity` rows (P3-N2).
pub struct StreamingRecorder<S: BatchSink> {
    capacity: usize,
    batch: Vec<MetricRow>,
    sink: S,
    total_rows: u64,
    flushes: u64,
}

impl<S: BatchSink> StreamingRecorder<S> {
    /// Create a recorder flushing every `capacity` rows. `capacity` is clamped
    /// to at least 1.
    pub fn new(capacity: usize, sink: S) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            batch: Vec::with_capacity(capacity),
            sink,
            total_rows: 0,
            flushes: 0,
        }
    }

    /// Record one row; flushes automatically when the batch fills. The in-RAM
    /// buffer never exceeds `capacity` (checked by [`Self::buffered`]).
    pub fn push(&mut self, row: MetricRow) {
        self.batch.push(row);
        self.total_rows += 1;
        if self.batch.len() >= self.capacity {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.batch.is_empty() {
            self.sink.write_batch(&self.batch);
            self.batch.clear();
            self.flushes += 1;
        }
    }

    /// Flush any partial batch and finalize the sink, returning it.
    pub fn finish(mut self) -> S {
        self.flush();
        self.sink.finish();
        self.sink
    }

    /// Rows currently held in RAM (always `<= capacity`).
    pub fn buffered(&self) -> usize {
        self.batch.len()
    }

    /// Total rows pushed over the recorder's life.
    pub fn total_rows(&self) -> u64 {
        self.total_rows
    }

    /// Number of batch flushes performed so far.
    pub fn flushes(&self) -> u64 {
        self.flushes
    }
}

/// Sum of a column in fixed left-to-right order (deterministic, P3-N3).
pub fn sum(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |acc, &v| acc + v)
}

/// Mean of a column in fixed order; `0.0` for an empty slice.
pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        sum(values) / values.len() as f64
    }
}

/// An in-memory sink that keeps every row: for tests and small runs.
#[derive(Default)]
pub struct VecSink {
    pub rows: Vec<MetricRow>,
    pub batches: usize,
    pub finished: bool,
}

impl BatchSink for VecSink {
    fn write_batch(&mut self, rows: &[MetricRow]) {
        assert!(!rows.is_empty(), "recorder must not emit empty batches");
        self.rows.extend_from_slice(rows);
        self.batches += 1;
    }

    fn finish(&mut self) {
        self.finished = true;
    }
}

/// A sink that appends rows as CSV lines into an owned `String`.
pub struct CsvSink {
    out: String,
}

impl Default for CsvSink {
    fn default() -> Self {
        Self::new()
    }
}

impl CsvSink {
    pub fn new() -> Self {
        let mut out = String::new();
        let _ = writeln!(out, "{}", MetricRow::COLUMNS.join(","));
        Self { out }
    }

    /// The accumulated CSV text (header + one line per row).
    pub fn into_string(self) -> String {
        self.out
    }
}

impl BatchSink for CsvSink {
    fn write_batch(&mut self, rows: &[MetricRow]) {
        for row in rows {
            let _ = writeln!(self.out, "{}", row.to_columns().join(","));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cycle: u64, node: Option<u32>, power: f64, temp: f64) -> MetricRow {
        MetricRow {
            cycle,
            node,
            latency: cycle as f64,
            throughput: 0.5,
            bandwidth: 4.0,
            utilization: 0.25,
            power_w: power,
            temp_c: temp,
        }
    }

    #[test]
    fn memory_stays_bounded_over_a_long_run() {
        let cap = 64;
        let mut rec = StreamingRecorder::new(cap, VecSink::default());
        let n = 100_000u64;
        for c in 0..n {
            rec.push(row(c, Some((c % 16) as u32), 1e-3, 40.0));
            // The core P3-N2 claim: RAM footprint never grows past one batch.
            assert!(rec.buffered() <= cap, "buffered {} > cap", rec.buffered());
        }
        assert_eq!(rec.total_rows(), n);
        let sink = rec.finish();
        // Every row reached the sink, in order, none dropped or duplicated.
        assert_eq!(sink.rows.len() as u64, n);
        assert!(sink
            .rows
            .iter()
            .enumerate()
            .all(|(i, r)| r.cycle == i as u64));
        assert!(sink.finished);
    }

    #[test]
    fn finish_flushes_the_partial_batch() {
        let mut rec = StreamingRecorder::new(10, VecSink::default());
        for c in 0..25 {
            rec.push(row(c, None, 0.0, 25.0));
        }
        // 2 full batches flushed, 5 rows still buffered.
        assert_eq!(rec.buffered(), 5);
        assert_eq!(rec.flushes(), 2);
        let sink = rec.finish();
        assert_eq!(sink.rows.len(), 25);
        assert_eq!(sink.batches, 3); // 10 + 10 + 5
    }

    #[test]
    fn capacity_is_clamped_to_at_least_one() {
        let mut rec = StreamingRecorder::new(0, VecSink::default());
        rec.push(row(0, None, 0.0, 25.0));
        // With capacity clamped to 1, each push flushes immediately.
        assert_eq!(rec.buffered(), 0);
        let sink = rec.finish();
        assert_eq!(sink.rows.len(), 1);
    }

    #[test]
    fn reductions_are_fixed_order_and_correct() {
        let vals = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(sum(&vals), 10.0);
        assert_eq!(mean(&vals), 2.5);
        assert_eq!(mean(&[]), 0.0);
        // Fixed left-to-right order => reproducible regardless of how the caller
        // parallelised collection.
        let a = sum(&vals);
        let b = vals.iter().fold(0.0, |acc, &v| acc + v);
        assert_eq!(a, b);
    }

    #[test]
    fn csv_sink_round_trips_rows() {
        let mut rec = StreamingRecorder::new(4, CsvSink::default());
        rec.push(row(0, Some(0), 1.5e-3, 41.5));
        rec.push(row(1, None, 2.0e-3, 42.0));
        let csv = rec.finish().into_string();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], MetricRow::COLUMNS.join(","));
        assert_eq!(lines.len(), 3); // header + 2 rows
        assert!(lines[1].starts_with("0,0,"));
        assert!(
            lines[2].starts_with("1,net,"),
            "network row uses 'net': {}",
            lines[2]
        );
    }
}
