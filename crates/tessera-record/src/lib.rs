//! Deterministic keyframe + re-simulation recorder (Phase 4,
//! `docs/phases/PHASE_4_PRD.md`): `SeekTo(cycle)` restores the nearest
//! keyframe and replays forward via [`tessera_core::Simulation::tick`] —
//! cheap and exact because the engine is deterministic, so "apply deltas up
//! to N" and "re-simulate to N" are the same mechanism. Re-simulation from a
//! fresh [`Simulation`] is both the fallback when no keyframe exists yet and
//! the replay-equivalence correctness oracle (gate G4.1): there is no stored
//! per-cycle state-diff format, only periodic keyframes plus the columnar
//! [`tessera_core::Event`] stream, which serves as the observable trace both
//! for analytics and for the oracle to diff bit-for-bit.
//!
//! This crate sits below `tessera-config`/`tessera-plugin`/`tessera-cli` in
//! the dependency direction (`docs/AGENTS.md`), so it never resolves a config
//! into trait objects itself — [`SimSource`] is the seam: an owning crate
//! (`tessera-cli`) implements it using its own config→registry construction,
//! and everything here ([`KeyframeWriter`]/[`KeyframeReader`]/[`SeekEngine`]/
//! the `replay` helpers) is generic over it.

mod error;
pub mod keyframe;
pub mod replay;
pub mod seek;

pub use error::RecordError;
pub use keyframe::{KeyframeReader, KeyframeWriter};
pub use replay::{first_divergence, run_collecting_events};
pub use seek::SeekEngine;

use tessera_core::Simulation;

/// Builds a fresh, cycle-0 [`Simulation`] the same way every time (same
/// topology, routing, traffic generator, seed) — the correctness precondition
/// every other item in this crate relies on. Implemented by the crate that
/// owns config→trait-object construction (`tessera-cli`), not by this one.
pub trait SimSource {
    fn fresh(&self) -> Simulation;
}
