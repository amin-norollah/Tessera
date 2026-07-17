//! Arrow/Parquet output for the streaming metric time-series (P3-F14).
//!
//! [`ParquetSink`] is a [`BatchSink`](crate::stream::BatchSink) that writes each
//! batch straight to a Parquet file as an Arrow `RecordBatch`, so it inherits the
//! recorder's bounded-memory property (P3-N2): only one batch is ever in RAM,
//! and the columnar file grows on disk. Gated behind the off-by-default
//! `parquet` cargo feature to keep default builds free of the Arrow dependency
//! tree.
//!
//! ```no_run
//! # #[cfg(feature = "parquet")] {
//! use tessera_stats::stream::{MetricRow, StreamingRecorder};
//! use tessera_stats::parquet_export::ParquetSink;
//!
//! let sink = ParquetSink::create("metrics.parquet", 4096).unwrap();
//! let mut rec = StreamingRecorder::new(4096, sink);
//! // rec.push(row) per interval ...
//! let sink = rec.finish();      // flushes + closes the Parquet footer
//! sink.into_result().unwrap();  // surface any write/close error
//! # }
//! ```

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::errors::ParquetError;
use parquet::file::properties::WriterProperties;

use crate::stream::{BatchSink, MetricRow};

/// The Arrow schema for a [`MetricRow`], column order matching
/// [`MetricRow::COLUMNS`].
pub fn schema() -> Schema {
    Schema::new(vec![
        Field::new("cycle", DataType::UInt64, false),
        Field::new("node", DataType::Utf8, false),
        Field::new("latency", DataType::Float64, false),
        Field::new("throughput", DataType::Float64, false),
        Field::new("bandwidth", DataType::Float64, false),
        Field::new("utilization", DataType::Float64, false),
        Field::new("power_w", DataType::Float64, false),
        Field::new("temp_c", DataType::Float64, false),
    ])
}

/// A [`BatchSink`] that streams metric batches to a Parquet file.
///
/// Errors are latched: the first write/close failure is stored and stops further
/// writes; surface it with [`ParquetSink::into_result`] after
/// [`StreamingRecorder::finish`](crate::stream::StreamingRecorder::finish).
pub struct ParquetSink {
    writer: Option<ArrowWriter<File>>,
    schema: SchemaRef,
    error: Option<ParquetError>,
}

impl ParquetSink {
    /// Create a Parquet file at `path`. `row_group_size` bounds the Parquet
    /// row-group size (rows); pass the recorder's batch capacity so each flushed
    /// batch maps cleanly onto a row group.
    pub fn create(path: impl AsRef<Path>, row_group_size: usize) -> Result<Self, ParquetError> {
        let schema = Arc::new(schema());
        let path = path.as_ref();
        let file = File::create(path)
            .map_err(|e| ParquetError::General(format!("create {}: {e}", path.display())))?;
        let props = WriterProperties::builder()
            .set_max_row_group_size(row_group_size.max(1))
            .build();
        let writer = ArrowWriter::try_new(file, schema.clone(), Some(props))?;
        Ok(Self {
            writer: Some(writer),
            schema,
            error: None,
        })
    }

    /// The latched error, if any write/close failed.
    pub fn error(&self) -> Option<&ParquetError> {
        self.error.as_ref()
    }

    /// Consume the sink, returning `Err` if any write/close failed.
    pub fn into_result(self) -> Result<(), ParquetError> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn record_batch(&self, rows: &[MetricRow]) -> Result<RecordBatch, ParquetError> {
        let cycle = UInt64Array::from(rows.iter().map(|r| r.cycle).collect::<Vec<_>>());
        let node = StringArray::from_iter_values(
            rows.iter()
                .map(|r| r.node.map_or_else(|| "net".to_string(), |n| n.to_string())),
        );
        let col =
            |f: fn(&MetricRow) -> f64| Float64Array::from(rows.iter().map(f).collect::<Vec<_>>());
        let columns: Vec<ArrayRef> = vec![
            Arc::new(cycle),
            Arc::new(node),
            Arc::new(col(|r| r.latency)),
            Arc::new(col(|r| r.throughput)),
            Arc::new(col(|r| r.bandwidth)),
            Arc::new(col(|r| r.utilization)),
            Arc::new(col(|r| r.power_w)),
            Arc::new(col(|r| r.temp_c)),
        ];
        RecordBatch::try_new(self.schema.clone(), columns)
            .map_err(|e| ParquetError::ArrowError(e.to_string()))
    }

    fn try_write(&mut self, rows: &[MetricRow]) -> Result<(), ParquetError> {
        let batch = self.record_batch(rows)?;
        self.writer
            .as_mut()
            .expect("writer present until finish()")
            .write(&batch)
    }
}

impl BatchSink for ParquetSink {
    fn write_batch(&mut self, rows: &[MetricRow]) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = self.try_write(rows) {
            self.error = Some(e);
        }
    }

    fn finish(&mut self) {
        if let Some(writer) = self.writer.take() {
            // Always close to release the file; only keep the error if the run
            // was otherwise clean (don't mask an earlier write failure).
            match writer.close() {
                Ok(_) => {}
                Err(e) if self.error.is_none() => self.error = Some(e),
                Err(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{MetricRow, StreamingRecorder};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tessera_stats_parquet_{name}.parquet"))
    }

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
    fn writes_a_parquet_file_that_reads_back() {
        let path = tmp("roundtrip");
        let n = 5_000u64;
        let sink = ParquetSink::create(&path, 512).unwrap();
        let mut rec = StreamingRecorder::new(512, sink);
        for c in 0..n {
            rec.push(row(c, Some((c % 8) as u32), 1e-3, 40.0 + (c % 5) as f64));
            assert!(rec.buffered() <= 512, "bounded memory preserved");
        }
        let sink = rec.finish();
        sink.into_result().expect("parquet write should succeed");

        // Read it back and verify every row is present, in order.
        let file = File::open(&path).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let mut total = 0u64;
        let mut first_cycle = None;
        for batch in reader {
            let batch = batch.unwrap();
            if first_cycle.is_none() {
                let cyc = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .unwrap();
                first_cycle = Some(cyc.value(0));
            }
            total += batch.num_rows() as u64;
            assert_eq!(batch.num_columns(), MetricRow::COLUMNS.len());
        }
        assert_eq!(total, n, "all rows persisted");
        assert_eq!(first_cycle, Some(0));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_matches_metric_row_columns() {
        let s = schema();
        assert_eq!(s.fields().len(), MetricRow::COLUMNS.len());
        for (field, name) in s.fields().iter().zip(MetricRow::COLUMNS) {
            assert_eq!(field.name(), name);
        }
    }
}
