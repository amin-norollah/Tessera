//! Event-trace helpers for the replay-equivalence gate (G4.1): drive a
//! [`Simulation`] while collecting its [`Event`] stream, and diagnose the
//! first place two traces diverge.

use tessera_core::{Event, Simulation};

/// Ticks `sim` from its current cycle to `until`, collecting every emitted
/// event. Turns event recording on if it wasn't already.
pub fn run_collecting_events(sim: &mut Simulation, until: u64) -> Vec<Event> {
    sim.record_events(true);
    let mut events = Vec::new();
    while sim.cycle < until {
        sim.tick();
        events.extend(sim.take_events());
    }
    events
}

/// The index of the first differing event between two traces, or `None` if
/// they're equal. A length mismatch reports the shorter trace's length (the
/// point one trace ran out before the other). Diagnostic for a failed
/// replay-equivalence assertion: points at exactly where two traces disagree
/// instead of just reporting `!=`.
pub fn first_divergence(a: &[Event], b: &[Event]) -> Option<usize> {
    a.iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_divergence_finds_the_exact_mismatch() {
        let a = vec![
            Event::ThrottleOn {
                cycle: 0,
                router: 0,
            },
            Event::ThrottleOn {
                cycle: 1,
                router: 0,
            },
        ];
        let mut b = a.clone();
        b[1] = Event::ThrottleOn {
            cycle: 1,
            router: 1,
        };
        assert_eq!(first_divergence(&a, &b), Some(1));
        assert_eq!(first_divergence(&a, &a), None);

        let c = vec![a[0]];
        assert_eq!(first_divergence(&a, &c), Some(1));
    }
}
