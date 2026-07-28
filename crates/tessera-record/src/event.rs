//! Arrow/Parquet export for the [`Event`] stream (P4-F4).
//!
//! [`Event`] is a variant-tagged enum; the columnar store flattens it into one
//! wide table (`kind` + every variant's fields as nullable columns) rather
//! than one table per variant, so a single Parquet file holds the whole
//! per-cycle trace and a query can `WHERE kind = '...'` to narrow to one
//! event type. [`EventParquetSink`] mirrors `tessera_stats::parquet_export`'s
//! `ParquetSink` (bounded-memory streaming write, latched write/close error),
//! implementing the same generic [`tessera_stats::stream::BatchSink`] this
//! crate reuses rather than forking.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::errors::ParquetError;
use parquet::file::properties::WriterProperties;
use tessera_core::Event;
use tessera_stats::stream::BatchSink;

/// Column names, in the order [`schema`] emits them.
pub const COLUMNS: [&str; 17] = [
    "kind",
    "cycle",
    "packet",
    "router",
    "in_port",
    "in_vc",
    "out_port",
    "out_vc",
    "cand_mask",
    "chosen",
    "flit",
    "src",
    "dst",
    "size",
    "latency",
    "port",
    "occ_milli",
];

/// The Arrow schema for an [`Event`]: `kind`/`cycle` are always present; every
/// other column is nullable and populated only by the variants that carry it.
pub fn schema() -> Schema {
    Schema::new(vec![
        Field::new("kind", DataType::Utf8, false),
        Field::new("cycle", DataType::UInt64, false),
        Field::new("packet", DataType::UInt32, true),
        Field::new("router", DataType::UInt32, true),
        Field::new("in_port", DataType::UInt32, true),
        Field::new("in_vc", DataType::UInt32, true),
        Field::new("out_port", DataType::UInt32, true),
        Field::new("out_vc", DataType::UInt32, true),
        Field::new("cand_mask", DataType::UInt32, true),
        Field::new("chosen", DataType::UInt32, true),
        Field::new("flit", DataType::UInt32, true),
        Field::new("src", DataType::UInt32, true),
        Field::new("dst", DataType::UInt32, true),
        Field::new("size", DataType::UInt32, true),
        Field::new("latency", DataType::UInt64, true),
        Field::new("port", DataType::UInt32, true),
        Field::new("occ_milli", DataType::UInt32, true),
    ])
}

fn kind(e: &Event) -> &'static str {
    match e {
        Event::Injected { .. } => "injected",
        Event::Routed { .. } => "routed",
        Event::VcAlloc { .. } => "vc_alloc",
        Event::SaGrant { .. } => "sa_grant",
        Event::Traversed { .. } => "traversed",
        Event::Delivered { .. } => "delivered",
        Event::Dropped { .. } => "dropped",
        Event::BufOccupancy { .. } => "buf_occupancy",
        Event::ThrottleOn { .. } => "throttle_on",
    }
}

fn cycle(e: &Event) -> u64 {
    match *e {
        Event::Injected { cycle, .. }
        | Event::Routed { cycle, .. }
        | Event::VcAlloc { cycle, .. }
        | Event::SaGrant { cycle, .. }
        | Event::Traversed { cycle, .. }
        | Event::Delivered { cycle, .. }
        | Event::Dropped { cycle, .. }
        | Event::BufOccupancy { cycle, .. }
        | Event::ThrottleOn { cycle, .. } => cycle,
    }
}

fn packet(e: &Event) -> Option<u32> {
    match *e {
        Event::Injected { packet, .. }
        | Event::Routed { packet, .. }
        | Event::VcAlloc { packet, .. }
        | Event::SaGrant { packet, .. }
        | Event::Traversed { packet, .. }
        | Event::Delivered { packet, .. }
        | Event::Dropped { packet, .. } => Some(packet),
        Event::BufOccupancy { .. } | Event::ThrottleOn { .. } => None,
    }
}

fn router(e: &Event) -> Option<u32> {
    match *e {
        Event::Routed { router, .. }
        | Event::VcAlloc { router, .. }
        | Event::SaGrant { router, .. }
        | Event::Traversed { router, .. }
        | Event::Dropped { router, .. }
        | Event::BufOccupancy { router, .. }
        | Event::ThrottleOn { router, .. } => Some(router),
        Event::Injected { .. } | Event::Delivered { .. } => None,
    }
}

fn in_port(e: &Event) -> Option<u32> {
    match *e {
        Event::Routed { in_port, .. }
        | Event::VcAlloc { in_port, .. }
        | Event::SaGrant { in_port, .. } => Some(in_port as u32),
        _ => None,
    }
}

fn in_vc(e: &Event) -> Option<u32> {
    match *e {
        Event::Routed { in_vc, .. }
        | Event::VcAlloc { in_vc, .. }
        | Event::SaGrant { in_vc, .. } => Some(in_vc as u32),
        _ => None,
    }
}

fn out_port(e: &Event) -> Option<u32> {
    match *e {
        Event::VcAlloc { out_port, .. }
        | Event::SaGrant { out_port, .. }
        | Event::Traversed { out_port, .. } => Some(out_port as u32),
        _ => None,
    }
}

fn out_vc(e: &Event) -> Option<u32> {
    match *e {
        Event::VcAlloc { out_vc, .. } | Event::Traversed { out_vc, .. } => Some(out_vc as u32),
        _ => None,
    }
}

fn cand_mask(e: &Event) -> Option<u32> {
    match *e {
        Event::Routed { cand_mask, .. } => Some(cand_mask as u32),
        _ => None,
    }
}

fn chosen(e: &Event) -> Option<u32> {
    match *e {
        Event::Routed { chosen, .. } => Some(chosen as u32),
        _ => None,
    }
}

fn flit(e: &Event) -> Option<u32> {
    match *e {
        Event::Traversed { flit, .. } => Some(flit),
        _ => None,
    }
}

fn src(e: &Event) -> Option<u32> {
    match *e {
        Event::Injected { src, .. } | Event::Delivered { src, .. } => Some(src),
        _ => None,
    }
}

fn dst(e: &Event) -> Option<u32> {
    match *e {
        Event::Injected { dst, .. } | Event::Delivered { dst, .. } => Some(dst),
        _ => None,
    }
}

fn size(e: &Event) -> Option<u32> {
    match *e {
        Event::Injected { size, .. } => Some(size as u32),
        _ => None,
    }
}

fn latency(e: &Event) -> Option<u64> {
    match *e {
        Event::Delivered { latency, .. } => Some(latency),
        _ => None,
    }
}

fn port(e: &Event) -> Option<u32> {
    match *e {
        Event::Dropped { port, .. } => Some(port as u32),
        _ => None,
    }
}

fn occ_milli(e: &Event) -> Option<u32> {
    match *e {
        Event::BufOccupancy { occ_milli, .. } => Some(occ_milli as u32),
        _ => None,
    }
}

/// A [`BatchSink<Event>`] that streams event batches to a Parquet file, the
/// same bounded-memory/latched-error shape as `tessera_stats::parquet_export
/// ::ParquetSink`.
pub struct EventParquetSink {
    writer: Option<ArrowWriter<File>>,
    schema: SchemaRef,
    error: Option<ParquetError>,
}

impl EventParquetSink {
    /// Create a Parquet file at `path`. `row_group_size` bounds the Parquet
    /// row-group size (rows); pass the recorder's batch capacity so each
    /// flushed batch maps cleanly onto a row group.
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

    fn record_batch(&self, rows: &[Event]) -> Result<RecordBatch, ParquetError> {
        let col_u32 = |f: fn(&Event) -> Option<u32>| {
            UInt32Array::from(rows.iter().map(f).collect::<Vec<_>>())
        };
        let col_u64 = |f: fn(&Event) -> Option<u64>| {
            UInt64Array::from(rows.iter().map(f).collect::<Vec<_>>())
        };
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from_iter_values(rows.iter().map(kind))),
            Arc::new(UInt64Array::from(
                rows.iter().map(cycle).collect::<Vec<_>>(),
            )),
            Arc::new(col_u32(packet)),
            Arc::new(col_u32(router)),
            Arc::new(col_u32(in_port)),
            Arc::new(col_u32(in_vc)),
            Arc::new(col_u32(out_port)),
            Arc::new(col_u32(out_vc)),
            Arc::new(col_u32(cand_mask)),
            Arc::new(col_u32(chosen)),
            Arc::new(col_u32(flit)),
            Arc::new(col_u32(src)),
            Arc::new(col_u32(dst)),
            Arc::new(col_u32(size)),
            Arc::new(col_u64(latency)),
            Arc::new(col_u32(port)),
            Arc::new(col_u32(occ_milli)),
        ];
        RecordBatch::try_new(self.schema.clone(), columns)
            .map_err(|e| ParquetError::ArrowError(e.to_string()))
    }

    fn try_write(&mut self, rows: &[Event]) -> Result<(), ParquetError> {
        let batch = self.record_batch(rows)?;
        self.writer
            .as_mut()
            .expect("writer present until finish()")
            .write(&batch)
    }
}

impl BatchSink<Event> for EventParquetSink {
    fn write_batch(&mut self, rows: &[Event]) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = self.try_write(rows) {
            self.error = Some(e);
        }
    }

    fn finish(&mut self) {
        if let Some(writer) = self.writer.take() {
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
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use tessera_stats::stream::StreamingRecorder;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tessera_record_event_parquet_{name}.parquet"))
    }

    fn sample_events() -> Vec<Event> {
        vec![
            Event::Injected {
                cycle: 0,
                packet: 1,
                src: 2,
                dst: 3,
                size: 4,
            },
            Event::Routed {
                cycle: 1,
                router: 2,
                in_port: 0,
                in_vc: 0,
                packet: 1,
                cand_mask: 0b110,
                chosen: 1,
            },
            Event::Delivered {
                cycle: 5,
                packet: 1,
                src: 2,
                dst: 3,
                latency: 5,
            },
            Event::ThrottleOn {
                cycle: 6,
                router: 4,
            },
        ]
    }

    #[test]
    fn schema_matches_column_list() {
        let s = schema();
        assert_eq!(s.fields().len(), COLUMNS.len());
        for (field, name) in s.fields().iter().zip(COLUMNS) {
            assert_eq!(field.name(), name);
        }
    }

    #[test]
    fn writes_a_parquet_file_that_reads_back_every_event() {
        let path = tmp("roundtrip");
        let events = sample_events();
        let sink = EventParquetSink::create(&path, 512).unwrap();
        let mut rec = StreamingRecorder::new(512, sink);
        for e in &events {
            rec.push(*e);
        }
        let sink = rec.finish();
        sink.into_result().expect("parquet write should succeed");

        let file = File::open(&path).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let mut kinds = Vec::new();
        let mut cycles = Vec::new();
        for batch in reader {
            let batch = batch.unwrap();
            assert_eq!(batch.num_columns(), COLUMNS.len());
            let kind_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let cycle_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap();
            for i in 0..batch.num_rows() {
                kinds.push(kind_col.value(i).to_string());
                cycles.push(cycle_col.value(i));
            }
        }
        assert_eq!(
            kinds,
            vec!["injected", "routed", "delivered", "throttle_on"]
        );
        assert_eq!(cycles, vec![0, 1, 5, 6]);

        let _ = std::fs::remove_file(&path);
    }
}
