//! DuckDB query layer over the event Parquet store (P4-F5, gate G4.3): opens
//! an in-process DuckDB connection with the store registered as a view named
//! `events`, so a caller runs plain SQL instead of hand-rolled aggregation
//! code. The trace store itself stays open Parquet (P4-N4) — DuckDB reads it
//! directly (`read_parquet`, no import step); this is a convenience layer
//! over it, not a proprietary format a caller is locked into.

use std::path::Path;

use duckdb::types::Value;
use duckdb::Connection;

use crate::RecordError;

/// One SQL query's result: column names, in order, and rows of dynamically-
/// typed values (DuckDB's own [`Value`], since a caller's SQL can select
/// anything).
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl QueryResult {
    /// The first row's first column as an `f64`, or `None` if the result is
    /// empty or that value isn't numeric. Convenience for a scalar aggregate
    /// query (e.g. `SELECT AVG(latency) FROM events`).
    pub fn scalar_f64(&self) -> Option<f64> {
        self.rows
            .first()
            .and_then(|row| row.first())
            .and_then(value_as_f64)
    }
}

/// Best-effort numeric coercion of a DuckDB [`Value`] to `f64`, covering the
/// integer/float variants a `SUM`/`AVG`/`COUNT` aggregate returns.
pub fn value_as_f64(v: &Value) -> Option<f64> {
    match *v {
        Value::TinyInt(i) => Some(i as f64),
        Value::SmallInt(i) => Some(i as f64),
        Value::Int(i) => Some(i as f64),
        Value::BigInt(i) => Some(i as f64),
        Value::HugeInt(i) => Some(i as f64),
        Value::UTinyInt(i) => Some(i as f64),
        Value::USmallInt(i) => Some(i as f64),
        Value::UInt(i) => Some(i as f64),
        Value::UBigInt(i) => Some(i as f64),
        Value::Float(f) => Some(f as f64),
        Value::Double(f) => Some(f),
        _ => None,
    }
}

/// Best-effort integer coercion, for grouping/bucket keys.
pub fn value_as_i64(v: &Value) -> Option<i64> {
    match *v {
        Value::TinyInt(i) => Some(i as i64),
        Value::SmallInt(i) => Some(i as i64),
        Value::Int(i) => Some(i as i64),
        Value::BigInt(i) => Some(i),
        Value::HugeInt(i) => Some(i as i64),
        Value::UTinyInt(i) => Some(i as i64),
        Value::USmallInt(i) => Some(i as i64),
        Value::UInt(i) => Some(i as i64),
        Value::UBigInt(i) => Some(i as i64),
        _ => None,
    }
}

/// An in-process DuckDB connection over one run's event Parquet store.
pub struct Query {
    conn: Connection,
}

impl Query {
    /// Opens an in-memory DuckDB connection with `event_store_path`
    /// registered as a view named `events`.
    pub fn open(event_store_path: impl AsRef<Path>) -> Result<Self, RecordError> {
        let conn = Connection::open_in_memory()?;
        let path = event_store_path
            .as_ref()
            .to_string_lossy()
            .replace('\'', "''");
        conn.execute_batch(&format!(
            "CREATE VIEW events AS SELECT * FROM read_parquet('{path}')"
        ))?;
        Ok(Self { conn })
    }

    /// Runs an arbitrary SQL query against `events` (and any other tables/
    /// views [`Self::execute`] has since created), returning every column
    /// and row.
    pub fn sql(&self, sql: &str) -> Result<QueryResult, RecordError> {
        let mut stmt = self.conn.prepare(sql)?;
        // `Statement::column_names` needs the query to have actually run
        // (DuckDB's arrow-based result only populates its schema once
        // `query`/`execute` fires); read it back off the `Rows` iterator's
        // statement handle rather than off `stmt` before that happens.
        let mut rows_iter = stmt.query([])?;
        let columns = rows_iter.as_ref().expect("just executed").column_names();
        let column_count = columns.len();
        let mut rows = Vec::new();
        while let Some(row) = rows_iter.next()? {
            let mut values = Vec::with_capacity(column_count);
            for i in 0..column_count {
                values.push(row.get::<usize, Value>(i)?);
            }
            rows.push(values);
        }
        Ok(QueryResult { columns, rows })
    }

    /// Runs a non-`SELECT` statement (e.g. another `CREATE VIEW`), for setup
    /// beyond the `events` view [`Self::open`] already registers.
    pub fn execute(&self, sql: &str) -> Result<(), RecordError> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }
}

// These tests need an actual event Parquet file, so they depend on the
// `events` feature too (`cargo test -p tessera-record --features query,events`);
// `query` alone still builds and tests cleanly without them.
#[cfg(all(test, feature = "events"))]
mod tests {
    use super::*;
    use crate::event::EventParquetSink;
    use tessera_core::Event;
    use tessera_stats::stream::StreamingRecorder;

    fn sample_events() -> Vec<Event> {
        vec![
            Event::Injected {
                cycle: 0,
                packet: 0,
                src: 0,
                dst: 1,
                size: 4,
            },
            Event::Delivered {
                cycle: 5,
                packet: 0,
                src: 0,
                dst: 1,
                latency: 5,
            },
            Event::Delivered {
                cycle: 9,
                packet: 1,
                src: 2,
                dst: 3,
                latency: 7,
            },
            Event::ThrottleOn {
                cycle: 10,
                router: 2,
            },
        ]
    }

    fn write_events(path: &std::path::Path, events: &[Event]) {
        let sink = EventParquetSink::create(path, 16).unwrap();
        let mut rec = StreamingRecorder::new(16, sink);
        for e in events {
            rec.push(*e);
        }
        rec.finish().into_result().unwrap();
    }

    #[test]
    fn sql_aggregate_matches_an_independently_computed_value() {
        let path = std::env::temp_dir().join(format!(
            "tessera_record_query_test_{}.parquet",
            std::process::id()
        ));
        write_events(&path, &sample_events());

        let query = Query::open(&path).unwrap();

        let count = query.sql("SELECT COUNT(*) FROM events").unwrap();
        assert_eq!(count.scalar_f64(), Some(4.0));

        let avg = query
            .sql("SELECT AVG(latency) FROM events WHERE kind = 'delivered'")
            .unwrap()
            .scalar_f64()
            .unwrap();
        // Cross-check against a manual aggregate computed independently in
        // Rust, not through DuckDB (the gate: G4.3/"query correctness").
        let delivered: Vec<u64> = sample_events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Delivered { latency, .. } => Some(latency),
                _ => None,
            })
            .collect();
        let manual_avg = delivered.iter().sum::<u64>() as f64 / delivered.len() as f64;
        assert_eq!(avg, manual_avg);

        std::fs::remove_file(&path).unwrap();
    }
}
