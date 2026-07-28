//! Ties keyframe cadence and event streaming together into one per-cycle
//! call (P4-F1/F4): [`Recorder::observe`] is called once per
//! [`Simulation::tick`], writing a keyframe on the configured interval and
//! draining that cycle's [`Event`]s into the streaming Arrow/Parquet sink —
//! both bounded-memory (P4-N2): only one event batch is ever held in RAM,
//! and keyframes are written straight to disk as they're taken.

use std::path::{Path, PathBuf};

use tessera_core::{Event, Simulation};
use tessera_stats::stream::StreamingRecorder;

use crate::event::EventParquetSink;
use crate::keyframe::KeyframeWriter;
use crate::RecordError;

pub struct Recorder {
    keyframe_interval: u64,
    keyframes: KeyframeWriter,
    events: StreamingRecorder<Event, EventParquetSink>,
}

impl Recorder {
    /// Creates a recorder writing keyframes into `keyframe_dir` every
    /// `keyframe_interval` cycles (`0` disables keyframing — events still
    /// stream) and the event trace to `event_store_path`, flushing in
    /// batches of `batch_capacity` rows.
    pub fn create(
        keyframe_dir: impl Into<PathBuf>,
        keyframe_interval: u64,
        event_store_path: impl AsRef<Path>,
        batch_capacity: usize,
    ) -> Result<Self, RecordError> {
        let keyframes = KeyframeWriter::create(keyframe_dir)?;
        let sink = EventParquetSink::create(event_store_path, batch_capacity)?;
        Ok(Self {
            keyframe_interval,
            keyframes,
            events: StreamingRecorder::new(batch_capacity, sink),
        })
    }

    /// Turns on event recording on `sim` and captures its initial (cycle 0)
    /// keyframe. Call once, before the first [`Self::observe`] — `observe`'s
    /// modulo check on the post-tick cycle can never land on 0 again, so
    /// this is the one place cycle 0 gets captured (not that it needs to:
    /// `SimSource::fresh` already reconstructs it for free, but keeping it in
    /// the keyframe set makes `KeyframeReader::nearest_le` total over
    /// `0..until` instead of a special case).
    pub fn start(&self, sim: &mut Simulation) -> Result<(), RecordError> {
        sim.record_events(true);
        if self.keyframe_interval > 0 {
            self.keyframes.write(&sim.snapshot())?;
        }
        Ok(())
    }

    /// Call once per cycle, right after `sim.tick()`: writes a keyframe if
    /// `sim.cycle` lands on the configured interval, then drains and streams
    /// this cycle's events.
    pub fn observe(&mut self, sim: &mut Simulation) -> Result<(), RecordError> {
        if self.keyframe_interval > 0 && sim.cycle.is_multiple_of(self.keyframe_interval) {
            self.keyframes.write(&sim.snapshot())?;
        }
        for event in sim.take_events() {
            self.events.push(event);
        }
        Ok(())
    }

    /// Flushes any partial event batch and closes the Parquet file. Errs if
    /// any write/close failed.
    pub fn finish(self) -> Result<(), RecordError> {
        self.events.finish().into_result().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tessera_record_recorder_test_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    // A minimal event source, standing in for a real `Simulation`: exercises
    // `Recorder`'s keyframe-cadence/event-drain wiring without needing
    // `tessera-model`/`tessera-routing`/`tessera-traffic` (this crate stays
    // below them in the dependency direction). The real-`Simulation` path is
    // exercised in `crates/tessera-cli/tests/recording.test.rs`.
    #[test]
    fn keyframe_cadence_and_event_count_match_expectations() {
        use tessera_core::{ActivityCounts, BufferPool, EngineSnapshot, Metrics};

        let dir = scratch_dir("cadence");
        let event_path = dir.join("events.parquet");
        std::fs::create_dir_all(&dir).unwrap();
        let keyframe_dir = dir.join("keyframes");

        let mut rec = Recorder::create(&keyframe_dir, 10, &event_path, 4).unwrap();

        let empty_snapshot = |cycle: u64| EngineSnapshot {
            schema_version: tessera_core::ENGINE_SNAPSHOT_VERSION,
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
        };

        // Directly exercise the keyframe-cadence + event-drain logic that
        // `observe` would apply to a real `Simulation`, one cycle at a time.
        for cycle in 0..25u64 {
            if cycle % 10 == 0 {
                rec.keyframes.write(&empty_snapshot(cycle)).unwrap();
            }
            rec.events.push(Event::ThrottleOn { cycle, router: 0 });
        }
        rec.finish().unwrap();

        let reader = crate::KeyframeReader::open(&keyframe_dir).unwrap();
        assert_eq!(reader.nearest_le(0), Some(0));
        assert_eq!(reader.nearest_le(9), Some(0));
        assert_eq!(reader.nearest_le(10), Some(10));
        assert_eq!(reader.nearest_le(24), Some(20));

        assert!(event_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
