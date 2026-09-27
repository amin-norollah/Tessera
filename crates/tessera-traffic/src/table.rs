//! Noxim table-based traffic (P1-F15): each source injects according to the
//! rows of a traffic table, each row `(src, dst, pir, por, t_on, t_off,
//! t_period)` active while `cycle % t_period` is in `[t_on, t_off)`.
//!
//! Mirrors Noxim's `GlobalTrafficTable::getCumulativePirPor` +
//! `ProcessingElement::canShot`: per cycle a source sums the rate of its
//! active rows into a cumulative threshold, draws one uniform `p`, injects
//! iff `p` falls under the threshold, and sends to the first active row whose
//! cumulative bound exceeds `p`. The rate summed is `pir` normally and `por`
//! when the source injected on the previous cycle (Noxim's burst knob).
//!
//! Rows are plain data here; `tessera-config::noxim` parses Noxim's text
//! format and the caller maps its rows onto [`TableRow`], keeping this crate
//! free of the config layer (`INITIAL_PRD.md` §20.1 dependency direction).

use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

use tessera_core::{NodeRng, PacketDesc, RouterId, TrafficGenerator};

/// One traffic-table row, in this run's node ids.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableRow {
    pub src: RouterId,
    pub dst: RouterId,
    /// Packet injection rate (packets/cycle) while the row is active.
    pub pir: f64,
    /// Rate used instead of `pir` on the cycle after the source injected.
    pub por: f64,
    pub t_on: u64,
    pub t_off: u64,
    pub t_period: u64,
}

#[derive(Clone, Debug, PartialEq, Error)]
pub enum TableTrafficError {
    #[error("traffic table row {row}: node {id} is out of range for a {num_nodes}-node network")]
    NodeOutOfRange { row: usize, id: u32, num_nodes: u32 },
    #[error(
        "traffic table row {row}: src and dst are both node {id}; a node cannot send to itself"
    )]
    SelfLoop { row: usize, id: u32 },
    #[error("traffic table row {row}: t_period must be >= 1")]
    ZeroPeriod { row: usize },
    #[error("traffic table row {row}: pir {pir} / por {por} must both be in [0.0, 1.0]")]
    RateOutOfRange { row: usize, pir: f64, por: f64 },
}

/// A [`TrafficGenerator`] replaying a Noxim traffic table.
pub struct TableTraffic {
    /// Rows grouped by source node, in table order (the order Noxim's
    /// cumulative destination walk uses).
    by_src: Vec<Vec<TableRow>>,
    packet_size: u16,
    /// Whether each node injected on its previous `on_tick`: selects `por`
    /// over `pir`. Path-dependent, so it is keyframed (P4-F1).
    shot_last: Vec<AtomicBool>,
}

impl TableTraffic {
    pub fn new(
        num_nodes: u32,
        rows: impl IntoIterator<Item = TableRow>,
        packet_size: u16,
    ) -> Result<Self, TableTrafficError> {
        let mut by_src = vec![Vec::new(); num_nodes as usize];
        for (row, r) in rows.into_iter().enumerate() {
            for id in [r.src.0, r.dst.0] {
                if id >= num_nodes {
                    return Err(TableTrafficError::NodeOutOfRange { row, id, num_nodes });
                }
            }
            if r.src == r.dst {
                return Err(TableTrafficError::SelfLoop { row, id: r.src.0 });
            }
            if r.t_period == 0 {
                return Err(TableTrafficError::ZeroPeriod { row });
            }
            if !(0.0..=1.0).contains(&r.pir) || !(0.0..=1.0).contains(&r.por) {
                return Err(TableTrafficError::RateOutOfRange {
                    row,
                    pir: r.pir,
                    por: r.por,
                });
            }
            by_src[r.src.index()].push(r);
        }
        Ok(Self {
            by_src,
            packet_size,
            shot_last: (0..num_nodes).map(|_| AtomicBool::new(false)).collect(),
        })
    }
}

impl TrafficGenerator for TableTraffic {
    fn on_tick(&self, cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        let rows = &self.by_src[node.index()];
        if rows.is_empty() {
            return;
        }
        let use_por = self.shot_last[node.index()].load(Ordering::Relaxed);
        // One draw per cycle regardless of outcome keeps the RNG stream's
        // consumption independent of which rows happen to be active.
        let p = rng.next_f64();
        let mut cumulative = 0.0;
        let mut dst = None;
        for r in rows {
            let phase = cycle % r.t_period;
            if phase < r.t_on || phase >= r.t_off {
                continue;
            }
            cumulative += if use_por { r.por } else { r.pir };
            if p < cumulative {
                dst = Some(r.dst);
                break;
            }
        }
        self.shot_last[node.index()].store(dst.is_some(), Ordering::Relaxed);
        if let Some(dst) = dst {
            out.push(PacketDesc {
                dst,
                size: self.packet_size,
                tag: 0,
            });
        }
    }

    fn snapshot(&self) -> Option<Vec<u8>> {
        Some(
            self.shot_last
                .iter()
                .map(|b| b.load(Ordering::Relaxed) as u8)
                .collect(),
        )
    }

    fn restore(&self, bytes: &[u8]) {
        for (slot, &b) in self.shot_last.iter().zip(bytes) {
            slot.store(b != 0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::RngStreams;

    fn row(src: u32, dst: u32, pir: f64) -> TableRow {
        TableRow {
            src: RouterId(src),
            dst: RouterId(dst),
            pir,
            por: pir,
            t_on: 0,
            t_off: 10,
            t_period: 10,
        }
    }

    fn draw(gen: &TableTraffic, node: u32, cycles: u64) -> Vec<(u64, RouterId)> {
        let mut rng = RngStreams::new(3).pe(node);
        let mut sent = Vec::new();
        for c in 0..cycles {
            let mut out = Vec::new();
            gen.on_tick(c, RouterId(node), &mut rng, &mut out);
            sent.extend(out.into_iter().map(|d| (c, d.dst)));
        }
        sent
    }

    #[test]
    fn only_sends_to_table_destinations() {
        let gen = TableTraffic::new(8, [row(0, 3, 0.2), row(0, 5, 0.3)], 4).unwrap();
        let sent = draw(&gen, 0, 5000);
        assert!(!sent.is_empty());
        assert!(sent
            .iter()
            .all(|(_, d)| *d == RouterId(3) || *d == RouterId(5)));
        let to3 = sent.iter().filter(|(_, d)| *d == RouterId(3)).count() as f64;
        let to5 = sent.len() as f64 - to3;
        // Rates 0.2 : 0.3 → destination split ~ 2 : 3.
        assert!((to3 / to5 - 2.0 / 3.0).abs() < 0.15, "{to3} vs {to5}");
        // Nodes without rows never inject.
        assert!(draw(&gen, 1, 1000).is_empty());
    }

    #[test]
    fn injection_rate_tracks_cumulative_pir() {
        let gen = TableTraffic::new(4, [row(0, 1, 0.1), row(0, 2, 0.15)], 4).unwrap();
        let rate = draw(&gen, 0, 20_000).len() as f64 / 20_000.0;
        assert!((rate - 0.25).abs() < 0.02, "rate {rate}");
    }

    #[test]
    fn rows_are_silent_outside_their_on_window() {
        let r = TableRow {
            t_on: 2,
            t_off: 5,
            t_period: 10,
            ..row(0, 1, 1.0)
        };
        let gen = TableTraffic::new(4, [r], 4).unwrap();
        for (c, _) in draw(&gen, 0, 100) {
            assert!((2..5).contains(&(c % 10)), "sent at cycle {c}");
        }
    }

    #[test]
    fn por_applies_on_the_cycle_after_a_shot() {
        // pir = 1 always fires; por = 0 then suppresses the next cycle, so
        // the source alternates shot / silent.
        let r = TableRow {
            por: 0.0,
            ..row(0, 1, 1.0)
        };
        let gen = TableTraffic::new(4, [r], 4).unwrap();
        let cycles: Vec<u64> = draw(&gen, 0, 10).into_iter().map(|(c, _)| c).collect();
        assert_eq!(cycles, [0, 2, 4, 6, 8]);
    }

    #[test]
    fn snapshot_round_trips_burst_state() {
        let r = TableRow {
            por: 0.0,
            ..row(0, 1, 1.0)
        };
        let gen = TableTraffic::new(4, [r], 4).unwrap();
        let mut rng = RngStreams::new(1).pe(0);
        gen.on_tick(0, RouterId(0), &mut rng, &mut Vec::new());
        let snap = gen.snapshot().unwrap();

        let fresh = TableTraffic::new(4, [r], 4).unwrap();
        fresh.restore(&snap);
        let mut out = Vec::new();
        fresh.on_tick(1, RouterId(0), &mut rng, &mut out);
        assert!(out.is_empty(), "restored burst state must select por = 0");
    }

    #[test]
    fn rejects_malformed_rows() {
        assert_eq!(
            TableTraffic::new(4, [row(0, 9, 0.1)], 4).err(),
            Some(TableTrafficError::NodeOutOfRange {
                row: 0,
                id: 9,
                num_nodes: 4
            })
        );
        assert_eq!(
            TableTraffic::new(4, [row(2, 2, 0.1)], 4).err(),
            Some(TableTrafficError::SelfLoop { row: 0, id: 2 })
        );
        let zero = TableRow {
            t_period: 0,
            ..row(0, 1, 0.1)
        };
        assert_eq!(
            TableTraffic::new(4, [zero], 4).err(),
            Some(TableTrafficError::ZeroPeriod { row: 0 })
        );
        assert!(matches!(
            TableTraffic::new(4, [row(0, 1, 1.5)], 4),
            Err(TableTrafficError::RateOutOfRange { .. })
        ));
    }
}
