//! Bimodal packet sizing: wraps any [`TrafficGenerator`], drawing each of its
//! packets' size from one of two classes (e.g. short control vs long data)
//! instead of using the wrapped generator's fixed size.

use tessera_core::{NodeRng, PacketDesc, RouterId, TrafficGenerator};

/// Overrides the wrapped generator's packet sizes with a two-class draw:
/// `data_ratio` of packets get `data` flits, the rest get `control`.
pub struct Bimodal<G> {
    inner: G,
    control: u16,
    data: u16,
    data_ratio: f64,
}

impl<G> Bimodal<G> {
    pub fn new(inner: G, control: u16, data: u16, data_ratio: f64) -> Self {
        assert!(
            control <= data,
            "control size ({control}) must be <= data size ({data})"
        );
        assert!(
            (0.0..=1.0).contains(&data_ratio),
            "data_ratio must be in [0, 1]"
        );
        Self {
            inner,
            control,
            data,
            data_ratio,
        }
    }
}

impl<G: TrafficGenerator> TrafficGenerator for Bimodal<G> {
    fn on_tick(&self, cycle: u64, node: RouterId, rng: &mut NodeRng, out: &mut Vec<PacketDesc>) {
        let start = out.len();
        self.inner.on_tick(cycle, node, rng, out);
        for desc in &mut out[start..] {
            desc.size = if rng.next_f64() < self.data_ratio {
                self.data
            } else {
                self.control
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Uniform;
    use tessera_core::RngStreams;

    #[test]
    fn draws_both_size_classes_over_many_cycles() {
        let inner = Uniform::new(16, 1.0, 4); // always injects, size irrelevant here
        let gen = Bimodal::new(inner, 1, 8, 0.5);
        let mut rng = RngStreams::new(1).pe(0);
        let mut out = Vec::new();
        let mut saw_control = false;
        let mut saw_data = false;
        for cycle in 0..200 {
            out.clear();
            gen.on_tick(cycle, RouterId(0), &mut rng, &mut out);
            for desc in &out {
                match desc.size {
                    1 => saw_control = true,
                    8 => saw_data = true,
                    other => panic!("unexpected size {other}"),
                }
            }
        }
        assert!(saw_control && saw_data, "expected both size classes");
    }

    #[test]
    fn zero_data_ratio_is_always_control_sized() {
        let inner = Uniform::new(16, 1.0, 4);
        let gen = Bimodal::new(inner, 2, 6, 0.0);
        let mut rng = RngStreams::new(2).pe(0);
        let mut out = Vec::new();
        for cycle in 0..50 {
            out.clear();
            gen.on_tick(cycle, RouterId(0), &mut rng, &mut out);
            for desc in &out {
                assert_eq!(desc.size, 2);
            }
        }
    }

    #[test]
    #[should_panic(expected = "must be <=")]
    fn rejects_control_larger_than_data() {
        Bimodal::new(Uniform::new(16, 1.0, 4), 8, 1, 0.5);
    }
}
