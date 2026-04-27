//! Importers for Noxim's legacy plain-text traffic and routing tables
//! (P1-F15). Each parses to a `Vec` of plain data rows over `tessera-core`'s
//! handle types; constructing an actual `TrafficGenerator`/`RoutingAlgorithm`
//! from those rows is the higher crates' job (`tessera-traffic`,
//! `tessera-routing`), keeping this crate's dependency footprint to
//! `tessera-core` only.
//!
//! Format (whitespace-separated columns, `#`-prefixed and blank lines
//! ignored):
//! - traffic table: `src dst pir por t_on t_off t_period`
//! - routing table: `src dst port`

use thiserror::Error;

use tessera_core::{PortId, RouterId};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoximTrafficRow {
    pub src: RouterId,
    pub dst: RouterId,
    /// Packet injection rate (packets/node/cycle) while the row is "on".
    pub pir: f64,
    /// Packet output ratio (fraction of injected traffic addressed here).
    pub por: f64,
    pub t_on: u64,
    pub t_off: u64,
    pub t_period: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoximRoutingRow {
    pub src: RouterId,
    pub dst: RouterId,
    pub port: PortId,
}

#[derive(Clone, Debug, PartialEq, Error)]
pub enum NoximImportError {
    #[error("line {line}: expected {expected} whitespace-separated fields, found {found}")]
    FieldCount {
        line: usize,
        expected: usize,
        found: usize,
    },
    #[error("line {line}, field '{field}': '{value}' is not a valid number")]
    InvalidNumber {
        line: usize,
        field: &'static str,
        value: String,
    },
}

fn field<T: std::str::FromStr>(
    line: usize,
    name: &'static str,
    raw: &str,
) -> Result<T, NoximImportError> {
    raw.parse().map_err(|_| NoximImportError::InvalidNumber {
        line,
        field: name,
        value: raw.to_string(),
    })
}

fn significant_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines().enumerate().filter_map(|(i, raw)| {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            None
        } else {
            Some((i + 1, line))
        }
    })
}

/// Parse a Noxim traffic table: `src dst pir por t_on t_off t_period` per row.
pub fn parse_traffic_table(text: &str) -> Result<Vec<NoximTrafficRow>, NoximImportError> {
    let mut rows = Vec::new();
    for (line, content) in significant_lines(text) {
        let cols: Vec<&str> = content.split_whitespace().collect();
        if cols.len() != 7 {
            return Err(NoximImportError::FieldCount {
                line,
                expected: 7,
                found: cols.len(),
            });
        }
        rows.push(NoximTrafficRow {
            src: RouterId(field(line, "src", cols[0])?),
            dst: RouterId(field(line, "dst", cols[1])?),
            pir: field(line, "pir", cols[2])?,
            por: field(line, "por", cols[3])?,
            t_on: field(line, "t_on", cols[4])?,
            t_off: field(line, "t_off", cols[5])?,
            t_period: field(line, "t_period", cols[6])?,
        });
    }
    Ok(rows)
}

/// Parse a Noxim routing table: `src dst port` per row.
pub fn parse_routing_table(text: &str) -> Result<Vec<NoximRoutingRow>, NoximImportError> {
    let mut rows = Vec::new();
    for (line, content) in significant_lines(text) {
        let cols: Vec<&str> = content.split_whitespace().collect();
        if cols.len() != 3 {
            return Err(NoximImportError::FieldCount {
                line,
                expected: 3,
                found: cols.len(),
            });
        }
        rows.push(NoximRoutingRow {
            src: RouterId(field(line, "src", cols[0])?),
            dst: RouterId(field(line, "dst", cols[1])?),
            port: PortId(field(line, "port", cols[2])?),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_traffic_table_ignoring_comments_and_blanks() {
        let text = "\
            # src dst pir  por t_on t_off t_period\n\
            \n\
            0   5   0.10  1.0 0    0     0\n\
            5   0   0.05  1.0 100  200   300\n\
        ";
        let rows = parse_traffic_table(text).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            NoximTrafficRow {
                src: RouterId(0),
                dst: RouterId(5),
                pir: 0.10,
                por: 1.0,
                t_on: 0,
                t_off: 0,
                t_period: 0,
            }
        );
        assert_eq!(rows[1].src, RouterId(5));
        assert_eq!(rows[1].t_period, 300);
    }

    #[test]
    fn parses_routing_table() {
        let text = "0 1 2\n1 0 3\n";
        let rows = parse_routing_table(text).unwrap();
        assert_eq!(
            rows,
            vec![
                NoximRoutingRow {
                    src: RouterId(0),
                    dst: RouterId(1),
                    port: PortId(2)
                },
                NoximRoutingRow {
                    src: RouterId(1),
                    dst: RouterId(0),
                    port: PortId(3)
                },
            ]
        );
    }

    #[test]
    fn reports_field_count_mismatch_with_line_number() {
        let err = parse_routing_table("0 1\n").unwrap_err();
        assert_eq!(
            err,
            NoximImportError::FieldCount {
                line: 1,
                expected: 3,
                found: 2
            }
        );
    }

    #[test]
    fn reports_invalid_number_with_line_and_field() {
        let err = parse_routing_table("0 1 not-a-port\n").unwrap_err();
        assert_eq!(
            err,
            NoximImportError::InvalidNumber {
                line: 1,
                field: "port",
                value: "not-a-port".to_string()
            }
        );
    }

    #[test]
    fn empty_table_is_not_an_error() {
        assert_eq!(parse_traffic_table("# nothing here\n").unwrap(), vec![]);
    }
}
