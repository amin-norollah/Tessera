//! Run comparison (P4-F6, gate G4.3): diff summary metrics and aligned
//! time-series between two runs, built on [`crate::query::Query`] rather
//! than a second data-access path.

use std::collections::BTreeSet;

use crate::query::{value_as_f64, value_as_i64, Query};
use crate::RecordError;

/// One named scalar metric, compared between two runs.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricDelta {
    pub metric: String,
    pub a: f64,
    pub b: f64,
    pub delta: f64,
}

/// One aligned time-series point: a bucket both runs are compared at (e.g. a
/// cycle range), with each run's value and their difference. A bucket only
/// one run has data for reports `0.0` for the other side.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeSeriesPoint {
    pub bucket: i64,
    pub a: f64,
    pub b: f64,
    pub delta: f64,
}

/// Summary + time-series diff between two runs (`a` is the baseline, `b` the
/// comparison).
#[derive(Debug, Clone, PartialEq)]
pub struct RunDiff {
    pub summary: Vec<MetricDelta>,
    pub time_series: Vec<TimeSeriesPoint>,
}

/// Computes summary-metric deltas: `metrics` is a list of `(label, sql)`
/// pairs, each `sql` a scalar aggregate query (e.g. `"SELECT AVG(latency)
/// FROM events WHERE kind = 'delivered'"`) run against both `a` and `b`.
pub fn compare_summary(
    a: &Query,
    b: &Query,
    metrics: &[(&str, &str)],
) -> Result<Vec<MetricDelta>, RecordError> {
    metrics
        .iter()
        .map(|&(label, sql)| {
            let va = a.sql(sql)?.scalar_f64().unwrap_or(0.0);
            let vb = b.sql(sql)?.scalar_f64().unwrap_or(0.0);
            Ok(MetricDelta {
                metric: label.to_string(),
                a: va,
                b: vb,
                delta: vb - va,
            })
        })
        .collect()
}

/// Computes an aligned time-series diff: `sql` must select exactly two
/// columns, a bucket key and a numeric value (e.g. `"SELECT cycle / 50,
/// AVG(latency) FROM events WHERE kind = 'delivered' GROUP BY 1"`), run
/// against both `a` and `b`. Buckets present in only one run's result are
/// aligned against `0.0` on the other side, so the two series compare over
/// their full combined range rather than silently truncating to the
/// shorter one.
pub fn compare_timeseries(
    a: &Query,
    b: &Query,
    sql: &str,
) -> Result<Vec<TimeSeriesPoint>, RecordError> {
    let series_a = bucketed_series(a, sql)?;
    let series_b = bucketed_series(b, sql)?;

    let buckets: BTreeSet<i64> = series_a.keys().chain(series_b.keys()).copied().collect();
    Ok(buckets
        .into_iter()
        .map(|bucket| {
            let va = series_a.get(&bucket).copied().unwrap_or(0.0);
            let vb = series_b.get(&bucket).copied().unwrap_or(0.0);
            TimeSeriesPoint {
                bucket,
                a: va,
                b: vb,
                delta: vb - va,
            }
        })
        .collect())
}

fn bucketed_series(
    query: &Query,
    sql: &str,
) -> Result<std::collections::BTreeMap<i64, f64>, RecordError> {
    let result = query.sql(sql)?;
    Ok(result
        .rows
        .iter()
        .filter_map(|row| {
            let bucket = value_as_i64(row.first()?)?;
            let value = value_as_f64(row.get(1)?)?;
            Some((bucket, value))
        })
        .collect())
}

/// Computes a full [`RunDiff`]: summary metrics plus one time-series.
pub fn compare(
    a: &Query,
    b: &Query,
    metrics: &[(&str, &str)],
    time_series_sql: &str,
) -> Result<RunDiff, RecordError> {
    Ok(RunDiff {
        summary: compare_summary(a, b, metrics)?,
        time_series: compare_timeseries(a, b, time_series_sql)?,
    })
}

#[cfg(all(test, feature = "events"))]
mod tests {
    use super::*;
    use crate::event::EventParquetSink;
    use tessera_core::Event;
    use tessera_stats::stream::StreamingRecorder;

    fn write_events(path: &std::path::Path, events: &[Event]) {
        let sink = EventParquetSink::create(path, 16).unwrap();
        let mut rec = StreamingRecorder::new(16, sink);
        for e in events {
            rec.push(*e);
        }
        rec.finish().into_result().unwrap();
    }

    fn delivered(cycle: u64, packet: u32, latency: u64) -> Event {
        Event::Delivered {
            cycle,
            packet,
            src: 0,
            dst: 1,
            latency,
        }
    }

    #[test]
    fn two_runs_differing_in_one_parameter_produce_a_correct_diff() {
        // Run "a": three deliveries, latencies 4, 6, 8 (avg 6).
        let path_a = std::env::temp_dir().join(format!(
            "tessera_record_compare_test_a_{}.parquet",
            std::process::id()
        ));
        write_events(
            &path_a,
            &[delivered(0, 0, 4), delivered(10, 1, 6), delivered(20, 2, 8)],
        );

        // Run "b": same shape, but every latency is 10 higher (a slower
        // network from one changed parameter): avg 16, delta +10.
        let path_b = std::env::temp_dir().join(format!(
            "tessera_record_compare_test_b_{}.parquet",
            std::process::id()
        ));
        write_events(
            &path_b,
            &[
                delivered(0, 0, 14),
                delivered(10, 1, 16),
                delivered(20, 2, 18),
            ],
        );

        let qa = Query::open(&path_a).unwrap();
        let qb = Query::open(&path_b).unwrap();

        let summary = compare_summary(
            &qa,
            &qb,
            &[
                (
                    "avg_latency",
                    "SELECT AVG(latency) FROM events WHERE kind = 'delivered'",
                ),
                (
                    "delivered_count",
                    "SELECT COUNT(*) FROM events WHERE kind = 'delivered'",
                ),
            ],
        )
        .unwrap();
        assert_eq!(summary[0].metric, "avg_latency");
        assert_eq!(summary[0].a, 6.0);
        assert_eq!(summary[0].b, 16.0);
        assert_eq!(summary[0].delta, 10.0);
        assert_eq!(summary[1].a, 3.0);
        assert_eq!(summary[1].b, 3.0);
        assert_eq!(summary[1].delta, 0.0);

        let time_series = compare_timeseries(
            &qa,
            &qb,
            "SELECT cycle, latency FROM events WHERE kind = 'delivered' ORDER BY cycle",
        )
        .unwrap();
        assert_eq!(time_series.len(), 3);
        for point in &time_series {
            assert_eq!(point.delta, 10.0, "every aligned bucket shifted by +10");
        }

        std::fs::remove_file(&path_a).unwrap();
        std::fs::remove_file(&path_b).unwrap();
    }
}
