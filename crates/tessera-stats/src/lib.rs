//! Metrics summarisation and export. Phase 0 produces latency-throughput sweep
//! points and writes them as CSV; Phase 3 adds the [`stream`] module: a
//! bounded-memory streaming time-series (P3-F14/N2) whose pluggable sink is the
//! seam for the columnar Arrow/Parquet store.

pub mod stream;

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use tessera_core::Metrics;

/// One point on a latency-throughput curve.
#[derive(Clone, Copy, Debug)]
pub struct RunPoint {
    /// Offered load in packets/node/cycle.
    pub offered_pkt_rate: f64,
    /// Offered load in flits/node/cycle.
    pub offered_flit_rate: f64,
    /// Accepted throughput in flits/node/cycle.
    pub accepted_flit_rate: f64,
    /// Mean end-to-end packet latency (cycles) over measured packets.
    pub avg_packet_latency: f64,
    pub injected_packets: u64,
    pub ejected_packets: u64,
}

impl RunPoint {
    pub fn from_metrics(
        m: &Metrics,
        nodes: u64,
        measure_cycles: u64,
        packet_size: u16,
        rate: f64,
    ) -> Self {
        let denom = (nodes * measure_cycles.max(1)) as f64;
        Self {
            offered_pkt_rate: rate,
            offered_flit_rate: rate * packet_size as f64,
            accepted_flit_rate: m.ejected_flits_in_window as f64 / denom,
            avg_packet_latency: m.avg_packet_latency(),
            injected_packets: m.injected_packets,
            ejected_packets: m.ejected_packets,
        }
    }
}

pub const CSV_HEADER: &str =
    "offered_pkt_rate,offered_flit_rate,accepted_flit_rate,avg_packet_latency,injected_packets,ejected_packets";

pub fn to_csv(points: &[RunPoint]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{CSV_HEADER}");
    for p in points {
        let _ = writeln!(
            s,
            "{:.6},{:.6},{:.6},{:.4},{},{}",
            p.offered_pkt_rate,
            p.offered_flit_rate,
            p.accepted_flit_rate,
            p.avg_packet_latency,
            p.injected_packets,
            p.ejected_packets,
        );
    }
    s
}

pub fn write_csv(path: &Path, points: &[RunPoint]) -> io::Result<()> {
    fs::write(path, to_csv(points))
}
