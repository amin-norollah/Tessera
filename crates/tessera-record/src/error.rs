//! [`RecordError`], this crate's single error type. Errors are values, not
//! panics (`docs/CODING_STANDARDS.md`).

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("keyframe I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("keyframe encode/decode error: {0}")]
    Codec(#[from] bincode::Error),
    #[error("keyframe schema version mismatch: found {found}, this build expects {expected}")]
    SchemaVersionMismatch { found: u16, expected: u16 },
    #[error("keyframe restore failed: {0}")]
    Restore(String),
    #[cfg(feature = "catalog")]
    #[error("catalog error: {0}")]
    Catalog(#[from] rusqlite::Error),
    #[cfg(feature = "events")]
    #[error("event stream error: {0}")]
    EventStream(#[from] parquet::errors::ParquetError),
    #[cfg(feature = "query")]
    #[error("query error: {0}")]
    Query(#[from] duckdb::Error),
}
