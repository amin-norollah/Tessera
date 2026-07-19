//! [`RecordError`]: this crate's single error type: errors are values, not
//! panics (`docs/AGENTS.md`).

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
}
