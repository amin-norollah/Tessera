//! Deterministic `SeekTo(cycle)` (P4-F2/F3): nearest-keyframe + replay when a
//! keyframe store is available, re-simulation from a fresh [`SimSource`]
//! otherwise. Re-simulation is also the replay-equivalence oracle (see
//! `crate::replay`).

use tessera_core::Simulation;

use crate::{KeyframeReader, RecordError, SimSource};

/// Reconstructs exact simulation state at an arbitrary cycle.
pub struct SeekEngine<'a, S: SimSource> {
    source: &'a S,
    keyframes: Option<&'a KeyframeReader>,
}

impl<'a, S: SimSource> SeekEngine<'a, S> {
    /// Re-simulation only (no keyframe store): every [`Self::seek_to`]
    /// replays from cycle 0.
    pub fn new(source: &'a S) -> Self {
        Self {
            source,
            keyframes: None,
        }
    }

    /// Nearest-keyframe + replay, falling back to re-simulation when
    /// `keyframes` has none at or before the requested cycle.
    pub fn with_keyframes(source: &'a S, keyframes: &'a KeyframeReader) -> Self {
        Self {
            source,
            keyframes: Some(keyframes),
        }
    }

    /// Re-simulates from a fresh [`SimSource`] to cycle `n` (P4-F3): the
    /// fallback path and the correctness oracle for [`Self::seek_to`].
    pub fn resimulate(&self, n: u64) -> Simulation {
        let mut sim = self.source.fresh();
        for _ in 0..n {
            sim.tick();
        }
        sim
    }

    /// Restores the nearest keyframe `<= n` (if any) and replays forward to
    /// `n`; otherwise re-simulates from cycle 0 (P4-F2).
    pub fn seek_to(&self, n: u64) -> Result<Simulation, RecordError> {
        let Some(reader) = self.keyframes else {
            return Ok(self.resimulate(n));
        };
        let Some(kf_cycle) = reader.nearest_le(n) else {
            return Ok(self.resimulate(n));
        };
        let snap = reader.read(kf_cycle)?;
        let mut sim = self.source.fresh();
        sim.restore(snap)
            .map_err(|e| RecordError::Restore(e.to_string()))?;
        for _ in kf_cycle..n {
            sim.tick();
        }
        Ok(sim)
    }
}
