//! SynFull import (P3.5-F3): a per-node Markov-chain traffic generator that
//! reproduces full-system NoC statistics at a fraction of trace-replay's cost
//! (RESEARCH_NOTES E4). Each node independently walks the same model's states
//! using its own RNG stream (P1-F18): per state, roll whether to inject (and
//! if so, sample a destination + bimodal size), then roll the transition to
//! the next state.
//!
//! # Format
//!
//! Reads a **Tessera-native TOML model** (`SynFullModelFile`), not the
//! original SynFull tool's MATLAB/C model-fitting output: this crate
//! consumes already-fitted Markov models (per this phase's stated scope: new
//! model *generation* from full-system runs is out of scope, that's SynFull's
//! own job), expressed as plain, hand-editable TOML in the same
//! cited-and-versioned spirit as `tessera-params`:
//!
//! ```toml
//! name = "example-app"
//! initial_state = "compute"
//!
//! [[states]]
//! name = "compute"
//! inject_prob = 0.05
//! size_class = "control"
//! dest = { kind = "uniform" }
//! next = [ { state = "compute", prob = 0.9 }, { state = "sync", prob = 0.1 } ]
//!
//! [[states]]
//! name = "sync"
//! inject_prob = 0.5
//! size_class = "data"
//! dest = { kind = "hotspot", node = 0, share = 0.8 }
//! next = [ { state = "compute", prob = 1.0 } ]
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Deserialize;
use tessera_core::{NodeRng, PacketDesc, RouterId, TrafficGenerator};

use crate::netrace::{MessageSizing, SizeClass};

const PROB_TOLERANCE: f64 = 1e-6;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DestPolicy {
    /// Uniform over every other node.
    Uniform,
    /// `share` of injections target `node`; the rest are uniform over the
    /// other nodes (mirrors [`crate::Hotspot`]).
    Hotspot { node: u32, share: f64 },
}

#[derive(Clone, Debug, Deserialize)]
struct TransitionFile {
    state: String,
    prob: f64,
}

#[derive(Clone, Debug, Deserialize)]
struct SynFullStateFile {
    name: String,
    inject_prob: f64,
    size_class: SizeClass,
    dest: DestPolicy,
    next: Vec<TransitionFile>,
}

#[derive(Clone, Debug, Deserialize)]
struct SynFullModelFile {
    name: String,
    initial_state: String,
    states: Vec<SynFullStateFile>,
}

#[derive(Debug, thiserror::Error)]
pub enum SynFullError {
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("model has no states")]
    Empty,
    #[error("unknown initial state '{0}'")]
    UnknownInitialState(String),
    #[error("state '{state}' transitions to unknown state '{target}'")]
    UnknownTransitionTarget { state: String, target: String },
    #[error("state '{state}' transition probabilities sum to {sum}, expected ~1.0")]
    TransitionProbabilitiesInvalid { state: String, sum: f64 },
    #[error("inject_prob for state '{state}' must be in [0, 1], found {value}")]
    InjectProbOutOfRange { state: String, value: f64 },
    #[error("hotspot node {node} out of range for a {num_nodes}-node mesh")]
    HotspotNodeOutOfRange { node: u32, num_nodes: u32 },
}

#[derive(Debug)]
struct ResolvedState {
    inject_prob: f64,
    size_class: SizeClass,
    dest: DestPolicy,
    /// Transition targets with cumulative probability bounds, e.g. `[(0.9,
    /// idx_a), (1.0, idx_b)]`: sampling draws one `f64` and takes the first
    /// entry whose cumulative bound it falls under.
    next_cumulative: Vec<(f64, usize)>,
}

/// A validated, dense-indexed Markov model ready to drive [`SynFull`].
#[derive(Debug)]
pub struct SynFullModel {
    pub name: String,
    states: Vec<ResolvedState>,
    initial: usize,
}

impl SynFullModel {
    pub fn from_toml(text: &str, num_nodes: u32) -> Result<Self, SynFullError> {
        let file: SynFullModelFile = toml::from_str(text)?;
        if file.states.is_empty() {
            return Err(SynFullError::Empty);
        }
        let index_of: HashMap<&str, usize> = file
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.as_str(), i))
            .collect();
        let initial =
            *index_of
                .get(file.initial_state.as_str())
                .ok_or(SynFullError::UnknownInitialState(
                    file.initial_state.clone(),
                ))?;

        let mut states = Vec::with_capacity(file.states.len());
        for s in &file.states {
            if !(0.0..=1.0).contains(&s.inject_prob) {
                return Err(SynFullError::InjectProbOutOfRange {
                    state: s.name.clone(),
                    value: s.inject_prob,
                });
            }
            if let DestPolicy::Hotspot { node, .. } = &s.dest {
                if *node >= num_nodes {
                    return Err(SynFullError::HotspotNodeOutOfRange {
                        node: *node,
                        num_nodes,
                    });
                }
            }
            let sum: f64 = s.next.iter().map(|t| t.prob).sum();
            if (sum - 1.0).abs() > PROB_TOLERANCE {
                return Err(SynFullError::TransitionProbabilitiesInvalid {
                    state: s.name.clone(),
                    sum,
                });
            }
            let mut cumulative = 0.0;
            let mut next_cumulative = Vec::with_capacity(s.next.len());
            for t in &s.next {
                let target = *index_of.get(t.state.as_str()).ok_or_else(|| {
                    SynFullError::UnknownTransitionTarget {
                        state: s.name.clone(),
                        target: t.state.clone(),
                    }
                })?;
                cumulative += t.prob;
                next_cumulative.push((cumulative, target));
            }
            states.push(ResolvedState {
                inject_prob: s.inject_prob,
                size_class: s.size_class,
                dest: s.dest.clone(),
                next_cumulative,
            });
        }

        Ok(Self {
            name: file.name,
            states,
            initial,
        })
    }
}

/// Trace/model identity (P3.5-F7): mirrors
/// `tessera_params::ParamDb::provenance` / `netrace::TraceProvenance`.
#[derive(Clone, Debug)]
pub struct ModelProvenance {
    pub source: String,
}

impl ModelProvenance {
    pub fn stamp(&self) -> String {
        format!("# Model provenance: SynFull TOML '{}'\n", self.source)
    }
}

/// A [`TrafficGenerator`] driving a per-node Markov chain sampled from a
/// [`SynFullModel`]. Every node shares the same model but walks its own
/// current-state index, advanced from its own [`NodeRng`] stream.
pub struct SynFull {
    model: SynFullModel,
    num_nodes: u32,
    sizing: MessageSizing,
    provenance: ModelProvenance,
    /// Current state index per node; `&self`-compatible via atomics (see
    /// `tessera_core::TrafficGenerator::on_tick`'s `&self` receiver).
    state: Vec<AtomicU32>,
}

impl SynFull {
    pub fn new(
        model: SynFullModel,
        num_nodes: u32,
        sizing: MessageSizing,
        provenance: ModelProvenance,
    ) -> Self {
        assert!(num_nodes >= 2, "SynFull traffic needs >= 2 nodes");
        let initial = model.initial as u32;
        Self {
            model,
            num_nodes,
            sizing,
            provenance,
            state: (0..num_nodes).map(|_| AtomicU32::new(initial)).collect(),
        }
    }

    pub fn provenance(&self) -> &ModelProvenance {
        &self.provenance
    }

    /// Current Markov state index for `node` (test/diagnostic observable).
    pub fn current_state(&self, node: RouterId) -> usize {
        self.state[node.index()].load(Ordering::Relaxed) as usize
    }

    fn sample_dest(&self, dest: &DestPolicy, node: RouterId, rng: &mut NodeRng) -> RouterId {
        let uniform_excluding_self = |rng: &mut NodeRng| {
            let mut d = rng.below(self.num_nodes - 1);
            if d >= node.0 {
                d += 1;
            }
            RouterId(d)
        };
        match *dest {
            DestPolicy::Uniform => uniform_excluding_self(rng),
            DestPolicy::Hotspot {
                node: hotspot,
                share,
            } => {
                if node.0 != hotspot && rng.next_f64() < share {
                    RouterId(hotspot)
                } else {
                    uniform_excluding_self(rng)
                }
            }
        }
    }

    fn sample_next(&self, current: &ResolvedState, rng: &mut NodeRng) -> usize {
        let r = rng.next_f64();
        current
            .next_cumulative
            .iter()
            .find(|&&(bound, _)| r < bound)
            .map(|&(_, idx)| idx)
            .unwrap_or_else(|| {
                current
                    .next_cumulative
                    .last()
                    .expect("validated non-empty")
                    .1
            })
    }
}

impl TrafficGenerator for SynFull {
    fn on_tick(&self, _cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        let idx = node.index();
        let cur = self.state[idx].load(Ordering::Relaxed) as usize;
        let st = &self.model.states[cur];
        if rng.next_f64() < st.inject_prob {
            let dst = self.sample_dest(&st.dest, node, rng);
            out.push(PacketDesc {
                dst,
                size: self.sizing.flits(st.size_class),
                tag: 0,
            });
        }
        let next = self.sample_next(st, rng);
        self.state[idx].store(next as u32, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Option<Vec<u8>> {
        let state: Vec<u32> = self
            .state
            .iter()
            .map(|a| a.load(Ordering::Relaxed))
            .collect();
        // A `Vec<u32>` always encodes.
        Some(bincode::serialize(&state).expect("Vec<u32> always serializes"))
    }

    fn restore(&self, bytes: &[u8]) {
        // See `Netrace::restore`: the caller has already validated `bytes`
        // came from this generator's own `snapshot`.
        let state: Vec<u32> = bincode::deserialize(bytes).expect("valid SynFull snapshot bytes");
        for (slot, value) in self.state.iter().zip(state) {
            slot.store(value, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::RngStreams;

    const SIMPLE_MODEL: &str = r#"
        name = "test-model"
        initial_state = "a"

        [[states]]
        name = "a"
        inject_prob = 1.0
        size_class = "control"
        dest = { kind = "uniform" }
        next = [ { state = "b", prob = 1.0 } ]

        [[states]]
        name = "b"
        inject_prob = 0.0
        size_class = "data"
        dest = { kind = "uniform" }
        next = [ { state = "a", prob = 1.0 } ]
    "#;

    fn sizing() -> MessageSizing {
        MessageSizing {
            control_flits: 1,
            data_flits: 4,
        }
    }

    #[test]
    fn parses_and_advances_through_states() {
        let model = SynFullModel::from_toml(SIMPLE_MODEL, 8).unwrap();
        let gen = SynFull::new(
            model,
            8,
            sizing(),
            ModelProvenance {
                source: "inline".into(),
            },
        );
        assert_eq!(gen.current_state(RouterId(0)), 0);

        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();
        gen.on_tick(0, RouterId(0), &mut rng, &mut out);
        assert_eq!(out.len(), 1, "state 'a' always injects");
        assert_eq!(out[0].size, 1, "state 'a' is control-sized");
        assert_eq!(gen.current_state(RouterId(0)), 1, "advanced to state 'b'");

        out.clear();
        gen.on_tick(1, RouterId(0), &mut rng, &mut out);
        assert!(out.is_empty(), "state 'b' never injects");
        assert_eq!(gen.current_state(RouterId(0)), 0, "advanced back to 'a'");
    }

    #[test]
    fn nodes_advance_independently() {
        let model = SynFullModel::from_toml(SIMPLE_MODEL, 8).unwrap();
        let gen = SynFull::new(
            model,
            8,
            sizing(),
            ModelProvenance {
                source: "inline".into(),
            },
        );
        let mut rng0 = RngStreams::new(1).pe(0);
        let mut out = Vec::new();
        gen.on_tick(0, RouterId(0), &mut rng0, &mut out);
        assert_eq!(gen.current_state(RouterId(0)), 1);
        assert_eq!(gen.current_state(RouterId(1)), 0, "node 1 untouched");
    }

    #[test]
    fn rejects_unbalanced_transition_probabilities() {
        let bad = r#"
            name = "bad"
            initial_state = "a"
            [[states]]
            name = "a"
            inject_prob = 0.5
            size_class = "control"
            dest = { kind = "uniform" }
            next = [ { state = "a", prob = 0.5 } ]
        "#;
        let err = SynFullModel::from_toml(bad, 8).unwrap_err();
        assert!(matches!(
            err,
            SynFullError::TransitionProbabilitiesInvalid { .. }
        ));
    }

    #[test]
    fn rejects_unknown_transition_target() {
        let bad = r#"
            name = "bad"
            initial_state = "a"
            [[states]]
            name = "a"
            inject_prob = 0.5
            size_class = "control"
            dest = { kind = "uniform" }
            next = [ { state = "nope", prob = 1.0 } ]
        "#;
        let err = SynFullModel::from_toml(bad, 8).unwrap_err();
        assert!(matches!(err, SynFullError::UnknownTransitionTarget { .. }));
    }

    #[test]
    fn rejects_hotspot_node_out_of_range() {
        let bad = r#"
            name = "bad"
            initial_state = "a"
            [[states]]
            name = "a"
            inject_prob = 0.5
            size_class = "control"
            dest = { kind = "hotspot", node = 99, share = 0.5 }
            next = [ { state = "a", prob = 1.0 } ]
        "#;
        let err = SynFullModel::from_toml(bad, 8).unwrap_err();
        assert!(matches!(
            err,
            SynFullError::HotspotNodeOutOfRange {
                node: 99,
                num_nodes: 8
            }
        ));
    }

    #[test]
    fn hotspot_destination_concentrates_traffic_over_many_cycles() {
        let hotspot_model = r#"
            name = "hot"
            initial_state = "a"
            [[states]]
            name = "a"
            inject_prob = 1.0
            size_class = "control"
            dest = { kind = "hotspot", node = 3, share = 1.0 }
            next = [ { state = "a", prob = 1.0 } ]
        "#;
        let model = SynFullModel::from_toml(hotspot_model, 8).unwrap();
        let gen = SynFull::new(
            model,
            8,
            sizing(),
            ModelProvenance {
                source: "inline".into(),
            },
        );
        let mut rng = RngStreams::new(1).pe(5);
        let mut out = Vec::new();
        for cycle in 0..20 {
            out.clear();
            gen.on_tick(cycle, RouterId(5), &mut rng, &mut out);
            assert_eq!(out[0].dst, RouterId(3));
        }
    }

    #[test]
    fn snapshot_and_restore_round_trips_the_markov_state() {
        let model = SynFullModel::from_toml(SIMPLE_MODEL, 8).unwrap();
        let gen = SynFull::new(
            model,
            8,
            sizing(),
            ModelProvenance {
                source: "inline".into(),
            },
        );
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();
        gen.on_tick(0, RouterId(0), &mut rng, &mut out);
        assert_eq!(gen.current_state(RouterId(0)), 1, "advanced to state 'b'");

        let bytes = gen.snapshot().expect("SynFull has stateful snapshot");

        // Restore onto a fresh generator built the same way `SimSource`
        // reconstruction would, not the original instance.
        let model = SynFullModel::from_toml(SIMPLE_MODEL, 8).unwrap();
        let restored = SynFull::new(
            model,
            8,
            sizing(),
            ModelProvenance {
                source: "inline".into(),
            },
        );
        assert_eq!(
            restored.current_state(RouterId(0)),
            0,
            "fresh instance starts at the initial state"
        );
        restored.restore(&bytes);
        assert_eq!(restored.current_state(RouterId(0)), 1);
        assert_eq!(
            restored.current_state(RouterId(1)),
            gen.current_state(RouterId(1))
        );
    }
}
