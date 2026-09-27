//! On-disk keyframe store (P4-F1): one file per keyframe, named by cycle,
//! bincode-encoded. Bincode, not the PRD's indicative rkyv: `EngineSnapshot`'s
//! fields are already plain `serde`-derivable data and keyframing isn't on the
//! per-cycle hot path, so round-trip simplicity and a small dependency
//! footprint matter more here than zero-copy reads. Nothing in
//! [`crate::SeekEngine`] or [`crate::SimSource`] is codec-specific, so
//! swapping this later doesn't ripple.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use tessera_core::{EngineSnapshot, ENGINE_SNAPSHOT_VERSION};

use crate::RecordError;

/// Keyframe payload compression (P4-F8): a space/write-cost tradeoff a
/// caller picks per keyframe store; see the `keyframe_tuning` example for
/// measured numbers across intervals and this choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    /// Raw bincode bytes.
    #[default]
    None,
    /// Gzip-compressed bincode bytes (smaller on disk, extra CPU per
    /// write/read).
    Gzip,
}

/// One-byte on-disk tag so [`KeyframeReader`] transparently reads either
/// compression a store was written with, without being told out of band.
const TAG_NONE: u8 = 0;
const TAG_GZIP: u8 = 1;

fn file_name(cycle: u64) -> String {
    format!("{cycle:020}.kf")
}

/// Writes periodic keyframes to a directory, one file per cycle.
pub struct KeyframeWriter {
    dir: PathBuf,
    compression: Compression,
}

impl KeyframeWriter {
    /// Creates (or reuses) `dir` as an uncompressed keyframe store.
    pub fn create(dir: impl Into<PathBuf>) -> Result<Self, RecordError> {
        Self::with_compression(dir, Compression::None)
    }

    /// As [`Self::create`], writing every keyframe with `compression`.
    pub fn with_compression(
        dir: impl Into<PathBuf>,
        compression: Compression,
    ) -> Result<Self, RecordError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, compression })
    }

    /// Writes `snap` as the keyframe for its `cycle`.
    pub fn write(&self, snap: &EngineSnapshot) -> Result<(), RecordError> {
        let bytes = bincode::serialize(snap)?;
        let mut out = Vec::with_capacity(bytes.len() + 1);
        match self.compression {
            Compression::None => {
                out.push(TAG_NONE);
                out.extend_from_slice(&bytes);
            }
            Compression::Gzip => {
                out.push(TAG_GZIP);
                let mut encoder = GzEncoder::new(out, flate2::Compression::default());
                encoder.write_all(&bytes)?;
                out = encoder.finish()?;
            }
        }
        std::fs::write(self.dir.join(file_name(snap.cycle)), out)?;
        Ok(())
    }
}

/// Reads keyframes back from a directory a [`KeyframeWriter`] populated.
pub struct KeyframeReader {
    dir: PathBuf,
    /// Discovered keyframe cycles, ascending.
    cycles: Vec<u64>,
}

impl KeyframeReader {
    /// Opens `dir`, discovering every keyframe already written there.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, RecordError> {
        let dir = dir.into();
        let mut cycles = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if let Some(cycle) = Self::parse_cycle(&entry.path()) {
                cycles.push(cycle);
            }
        }
        cycles.sort_unstable();
        Ok(Self { dir, cycles })
    }

    fn parse_cycle(path: &Path) -> Option<u64> {
        path.file_stem()?.to_str()?.parse::<u64>().ok()
    }

    /// The largest recorded keyframe cycle `<= n`, or `None` if the store has
    /// none (the caller falls back to re-simulating from a fresh
    /// [`crate::SimSource`]).
    pub fn nearest_le(&self, n: u64) -> Option<u64> {
        self.cycles.iter().rev().find(|&&c| c <= n).copied()
    }

    /// Reads the keyframe recorded at exactly `cycle` (see [`Self::nearest_le`]),
    /// transparently decompressing regardless of which [`Compression`] it was
    /// written with.
    pub fn read(&self, cycle: u64) -> Result<EngineSnapshot, RecordError> {
        let raw = std::fs::read(self.dir.join(file_name(cycle)))?;
        let (&tag, payload) = raw.split_first().ok_or_else(|| {
            RecordError::Codec(bincode::ErrorKind::Custom("empty keyframe file".to_string()).into())
        })?;
        let bytes = match tag {
            TAG_NONE => payload.to_vec(),
            TAG_GZIP => {
                let mut decoder = GzDecoder::new(payload);
                let mut decompressed = Vec::new();
                decoder.read_to_end(&mut decompressed)?;
                decompressed
            }
            other => {
                return Err(RecordError::Codec(
                    bincode::ErrorKind::Custom(format!("unknown keyframe compression tag {other}"))
                        .into(),
                ))
            }
        };
        let snap: EngineSnapshot = bincode::deserialize(&bytes)?;
        if snap.schema_version != ENGINE_SNAPSHOT_VERSION {
            return Err(RecordError::SchemaVersionMismatch {
                found: snap.schema_version,
                expected: ENGINE_SNAPSHOT_VERSION,
            });
        }
        Ok(snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::{ActivityCounts, BufferPool, Metrics};

    fn empty_snapshot(cycle: u64) -> EngineSnapshot {
        EngineSnapshot {
            schema_version: ENGINE_SNAPSHOT_VERSION,
            cycle,
            metrics: Metrics::default(),
            measure_start: 0,
            measure_end: 0,
            next_packet_id: 0,
            injecting: true,
            arena_headers: Vec::new(),
            arena_free: Vec::new(),
            buffers: BufferPool::new(1, 1, 1, 1).snapshot(),
            reserve_port: Vec::new(),
            reserve_vc: Vec::new(),
            hold: Vec::new(),
            credits: Vec::new(),
            links: Vec::new(),
            credit_returns: Vec::new(),
            activity: vec![ActivityCounts::default()],
            pe_src: Vec::new(),
            pe_rng: Vec::new(),
            router_rng: Vec::new(),
            traffic_state: None,
            va_alloc_state: None,
            sa1_alloc_state: None,
            sa2_alloc_state: None,
            selection_state: None,
        }
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tessera_record_keyframe_test_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trips_and_finds_the_nearest_keyframe() {
        let dir = scratch_dir("roundtrip");
        let writer = KeyframeWriter::create(&dir).unwrap();
        for cycle in [0, 100, 200] {
            writer.write(&empty_snapshot(cycle)).unwrap();
        }

        let reader = KeyframeReader::open(&dir).unwrap();
        assert_eq!(reader.nearest_le(50), Some(0));
        assert_eq!(reader.nearest_le(150), Some(100));
        assert_eq!(reader.nearest_le(200), Some(200));
        assert_eq!(reader.nearest_le(999), Some(200));

        let snap = reader.read(100).unwrap();
        assert_eq!(snap.cycle, 100);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nearest_le_is_none_before_the_first_keyframe() {
        let dir = scratch_dir("empty");
        let writer = KeyframeWriter::create(&dir).unwrap();
        writer.write(&empty_snapshot(100)).unwrap();

        let reader = KeyframeReader::open(&dir).unwrap();
        assert_eq!(reader.nearest_le(50), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gzip_compression_round_trips_and_shrinks_a_compressible_payload() {
        let mut snap = empty_snapshot(42);
        snap.reserve_port = vec![7; 10_000];

        let dir = scratch_dir("gzip");
        let writer = KeyframeWriter::with_compression(&dir, Compression::Gzip).unwrap();
        writer.write(&snap).unwrap();

        let reader = KeyframeReader::open(&dir).unwrap();
        let round_tripped = reader.read(42).unwrap();
        assert_eq!(round_tripped.cycle, 42);
        assert_eq!(round_tripped.reserve_port, snap.reserve_port);
        let compressed_size = std::fs::metadata(dir.join(file_name(42))).unwrap().len();

        let dir_plain = scratch_dir("gzip_baseline");
        let writer_plain = KeyframeWriter::create(&dir_plain).unwrap();
        writer_plain.write(&snap).unwrap();
        let plain_size = std::fs::metadata(dir_plain.join(file_name(42)))
            .unwrap()
            .len();

        assert!(
            compressed_size < plain_size,
            "gzip ({compressed_size}B) should shrink a highly repetitive payload vs raw ({plain_size}B)"
        );

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&dir_plain).unwrap();
    }
}
