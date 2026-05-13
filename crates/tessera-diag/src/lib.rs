//! `tessera-diag` — network-health diagnostics (P3-F15/F16).
//!
//! Detectors for the four classic NoC pathologies, plus invariant checks, run
//! over **observable state only** (the read frame): they never touch the
//! datapath, so they have zero effect on simulation results (INITIAL_PRD §9A,
//! "diagnostics run on the read frame").
//!
//! Each detector consumes a stream of [`HealthSample`]s (one per diagnostic
//! interval) and emits structured [`DiagEvent`]s carrying a [`DiagKind`], the
//! [`Location`], and the cycle it fired:
//!
//! - [`DeadlockDetector`] — no delivery **and no movement** for N cycles while
//!   flits are in flight (cyclic wait / no-progress).
//! - [`LivelockDetector`] — no delivery but flits **keep moving** (hops rising)
//!   for N cycles: packets circulating without being delivered.
//! - [`CongestionDetector`] — a router's buffer occupancy stays saturated for N
//!   cycles (a hotspot).
//! - [`StarvationDetector`] — a flow has pending demand but is never granted for
//!   N cycles.
//!
//! [`InvariantChecker`] provides the "verify mode" checks (P3-F16): flit
//! conservation (no-flit-loss) and monotonic cumulative counters.

use tessera_core::RouterId;

/// A snapshot of observable network state at one diagnostic interval. All
/// `*_flits` / `total_hops` counters are **cumulative** since the run started.
#[derive(Clone, Debug, Default)]
pub struct HealthSample {
    pub cycle: u64,
    /// Cumulative flits delivered (ejected) so far.
    pub delivered_flits: u64,
    /// Cumulative hop count across all flits — the network-wide movement
    /// indicator that separates livelock (rising) from deadlock (frozen).
    pub total_hops: u64,
    /// Cumulative flits injected so far.
    pub injected_flits: u64,
    /// Cumulative flits dropped (lossy configs; 0 in a lossless run).
    pub dropped_flits: u64,
    /// Flits currently in the network (injected but not yet delivered/dropped).
    pub in_flight_flits: u64,
    /// Per-router mean buffer occupancy in `0.0..=1.0`.
    pub router_occupancy: Vec<f32>,
    /// Per-flow: does the flow currently have a packet awaiting a grant?
    pub flow_pending: Vec<bool>,
    /// Per-flow: was the flow granted a resource since the previous sample?
    pub flow_granted: Vec<bool>,
}

/// The pathology a [`DiagEvent`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagKind {
    Deadlock,
    Livelock,
    Congestion,
    Starvation,
}

/// Where a [`DiagEvent`] is located.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// Network-wide (deadlock / livelock).
    Network,
    Router(RouterId),
    /// A traffic flow, identified by its index in the sample's per-flow vectors.
    Flow(u32),
}

/// A structured diagnostic finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagEvent {
    pub kind: DiagKind,
    pub location: Location,
    /// Cycle at which the condition crossed its detection window.
    pub cycle: u64,
    pub detail: String,
}

/// A detector consumes one sample at a time and returns any events it fires.
pub trait Detector {
    fn observe(&mut self, sample: &HealthSample) -> Vec<DiagEvent>;
}

/// Shared "no-progress" bookkeeping: tracks when the current stall episode
/// began and whether we have already fired for it (latched until progress
/// resumes), so one episode yields exactly one event.
#[derive(Clone, Copy, Debug, Default)]
struct Stall {
    since: Option<u64>,
    latched: bool,
    prev_delivered: Option<u64>,
    prev_hops: Option<u64>,
}

impl Stall {
    /// Feed a sample; returns `(delivered_progress, moved, fire_cycle)` where
    /// `fire_cycle` is `Some` only on the sample that first crosses `window`
    /// while `condition` holds.
    fn update(&mut self, sample: &HealthSample, condition: bool, window: u64) -> Option<u64> {
        let fire = if condition {
            let since = *self.since.get_or_insert(sample.cycle);
            if !self.latched && sample.cycle.saturating_sub(since) >= window {
                self.latched = true;
                Some(sample.cycle)
            } else {
                None
            }
        } else {
            self.since = None;
            self.latched = false;
            None
        };
        self.prev_delivered = Some(sample.delivered_flits);
        self.prev_hops = Some(sample.total_hops);
        fire
    }

    fn delivered_progress(&self, sample: &HealthSample) -> bool {
        self.prev_delivered
            .is_none_or(|prev| sample.delivered_flits > prev)
    }

    fn moved(&self, sample: &HealthSample) -> bool {
        self.prev_hops.is_none_or(|prev| sample.total_hops > prev)
    }
}

/// Counts saturated / high-occupancy routers, for event detail.
fn saturated_routers(sample: &HealthSample, threshold: f32) -> Vec<usize> {
    sample
        .router_occupancy
        .iter()
        .enumerate()
        .filter(|(_, &o)| o >= threshold)
        .map(|(i, _)| i)
        .collect()
}

/// Fires when nothing is delivered **and nothing moves** for `window` cycles
/// while flits remain in flight (P3-F15 deadlock).
#[derive(Clone, Debug)]
pub struct DeadlockDetector {
    window: u64,
    occupancy_threshold: f32,
    stall: Stall,
}

impl DeadlockDetector {
    pub fn new(window_cycles: u64) -> Self {
        Self {
            window: window_cycles,
            occupancy_threshold: 0.9,
            stall: Stall::default(),
        }
    }
}

impl Detector for DeadlockDetector {
    fn observe(&mut self, sample: &HealthSample) -> Vec<DiagEvent> {
        let frozen = !self.stall.delivered_progress(sample)
            && !self.stall.moved(sample)
            && sample.in_flight_flits > 0;
        match self.stall.update(sample, frozen, self.window) {
            Some(cycle) => {
                let stuck = saturated_routers(sample, self.occupancy_threshold);
                vec![DiagEvent {
                    kind: DiagKind::Deadlock,
                    location: Location::Network,
                    cycle,
                    detail: format!(
                        "no delivery or movement for >= {} cycles with {} flits in flight; \
                         {} saturated router(s): {:?}",
                        self.window,
                        sample.in_flight_flits,
                        stuck.len(),
                        stuck
                    ),
                }]
            }
            None => Vec::new(),
        }
    }
}

/// Fires when nothing is delivered but flits **keep moving** for `window`
/// cycles: packets circulating without progress (P3-F15 livelock).
#[derive(Clone, Debug)]
pub struct LivelockDetector {
    window: u64,
    stall: Stall,
}

impl LivelockDetector {
    pub fn new(window_cycles: u64) -> Self {
        Self {
            window: window_cycles,
            stall: Stall::default(),
        }
    }
}

impl Detector for LivelockDetector {
    fn observe(&mut self, sample: &HealthSample) -> Vec<DiagEvent> {
        let circulating = !self.stall.delivered_progress(sample)
            && self.stall.moved(sample)
            && sample.in_flight_flits > 0;
        match self.stall.update(sample, circulating, self.window) {
            Some(cycle) => vec![DiagEvent {
                kind: DiagKind::Livelock,
                location: Location::Network,
                cycle,
                detail: format!(
                    "{} flits circulating (hops rising) with no delivery for >= {} cycles",
                    sample.in_flight_flits, self.window
                ),
            }],
            None => Vec::new(),
        }
    }
}

/// Per-router episodic tracker for sustained-condition detectors.
#[derive(Clone, Debug, Default)]
struct Episodes {
    since: Vec<Option<u64>>,
    latched: Vec<bool>,
}

impl Episodes {
    fn ensure(&mut self, n: usize) {
        if self.since.len() < n {
            self.since.resize(n, None);
            self.latched.resize(n, false);
        }
    }

    /// Update entity `i` with whether its condition holds this sample; returns
    /// the fire cycle on the sample that first crosses `window`.
    fn update(&mut self, i: usize, active: bool, cycle: u64, window: u64) -> Option<u64> {
        if active {
            let since = *self.since[i].get_or_insert(cycle);
            if !self.latched[i] && cycle.saturating_sub(since) >= window {
                self.latched[i] = true;
                return Some(cycle);
            }
        } else {
            self.since[i] = None;
            self.latched[i] = false;
        }
        None
    }
}

/// Fires when a router's buffer occupancy stays at/above `threshold` for
/// `window` cycles (P3-F15 congestion / hotspot).
#[derive(Clone, Debug)]
pub struct CongestionDetector {
    window: u64,
    threshold: f32,
    episodes: Episodes,
}

impl CongestionDetector {
    pub fn new(window_cycles: u64, occupancy_threshold: f32) -> Self {
        Self {
            window: window_cycles,
            threshold: occupancy_threshold,
            episodes: Episodes::default(),
        }
    }
}

impl Detector for CongestionDetector {
    fn observe(&mut self, sample: &HealthSample) -> Vec<DiagEvent> {
        self.episodes.ensure(sample.router_occupancy.len());
        let mut events = Vec::new();
        for (i, &occ) in sample.router_occupancy.iter().enumerate() {
            if let Some(cycle) =
                self.episodes
                    .update(i, occ >= self.threshold, sample.cycle, self.window)
            {
                events.push(DiagEvent {
                    kind: DiagKind::Congestion,
                    location: Location::Router(RouterId(i as u32)),
                    cycle,
                    detail: format!(
                        "router {i} occupancy {occ:.2} >= {:.2} for >= {} cycles",
                        self.threshold, self.window
                    ),
                });
            }
        }
        events
    }
}

/// Fires when a flow has pending demand but is never granted for `window`
/// cycles (P3-F15 starvation).
#[derive(Clone, Debug)]
pub struct StarvationDetector {
    window: u64,
    episodes: Episodes,
}

impl StarvationDetector {
    pub fn new(window_cycles: u64) -> Self {
        Self {
            window: window_cycles,
            episodes: Episodes::default(),
        }
    }
}

impl Detector for StarvationDetector {
    fn observe(&mut self, sample: &HealthSample) -> Vec<DiagEvent> {
        let n = sample.flow_pending.len();
        self.episodes.ensure(n);
        let mut events = Vec::new();
        for i in 0..n {
            let granted = sample.flow_granted.get(i).copied().unwrap_or(false);
            let starving = sample.flow_pending[i] && !granted;
            if let Some(cycle) = self.episodes.update(i, starving, sample.cycle, self.window) {
                events.push(DiagEvent {
                    kind: DiagKind::Starvation,
                    location: Location::Flow(i as u32),
                    cycle,
                    detail: format!(
                        "flow {i} pending but ungranted for >= {} cycles",
                        self.window
                    ),
                });
            }
        }
        events
    }
}

/// A broken invariant found in "verify mode" (P3-F16).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// `injected != delivered + dropped + in_flight` — a flit was lost or
    /// conjured.
    FlitConservation {
        cycle: u64,
        injected: u64,
        accounted: u64,
    },
    /// A cumulative counter went backwards between samples.
    NonMonotonic {
        cycle: u64,
        counter: &'static str,
        prev: u64,
        now: u64,
    },
}

/// Stateful invariant checker (P3-F16): every flit is accounted for and the
/// cumulative counters never decrease. Run over the sample stream in verify
/// mode; any [`Violation`] is a bug in the datapath or the stats.
#[derive(Clone, Debug, Default)]
pub struct InvariantChecker {
    prev_injected: Option<u64>,
    prev_delivered: Option<u64>,
    prev_hops: Option<u64>,
}

impl InvariantChecker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check flit conservation for a single sample, independent of history.
    pub fn check_flit_conservation(sample: &HealthSample) -> Result<(), Violation> {
        let accounted = sample.delivered_flits + sample.dropped_flits + sample.in_flight_flits;
        if sample.injected_flits == accounted {
            Ok(())
        } else {
            Err(Violation::FlitConservation {
                cycle: sample.cycle,
                injected: sample.injected_flits,
                accounted,
            })
        }
    }

    /// Feed a sample; returns every invariant it violates.
    pub fn observe(&mut self, sample: &HealthSample) -> Vec<Violation> {
        let mut v = Vec::new();
        if let Err(e) = Self::check_flit_conservation(sample) {
            v.push(e);
        }
        let mut mono = |counter, prev: &mut Option<u64>, now: u64| {
            if let Some(p) = *prev {
                if now < p {
                    v.push(Violation::NonMonotonic {
                        cycle: sample.cycle,
                        counter,
                        prev: p,
                        now,
                    });
                }
            }
            *prev = Some(now);
        };
        mono(
            "injected_flits",
            &mut self.prev_injected,
            sample.injected_flits,
        );
        mono(
            "delivered_flits",
            &mut self.prev_delivered,
            sample.delivered_flits,
        );
        mono("total_hops", &mut self.prev_hops, sample.total_hops);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A healthy sample at `cycle`: everything delivered eventually, no stall.
    fn healthy(cycle: u64, delivered: u64, hops: u64) -> HealthSample {
        HealthSample {
            cycle,
            delivered_flits: delivered,
            total_hops: hops,
            injected_flits: delivered + 10,
            in_flight_flits: 10,
            ..Default::default()
        }
    }

    #[test]
    fn deadlock_fires_only_after_the_window_and_once_per_episode() {
        let mut d = DeadlockDetector::new(100);
        // Healthy progress: no events.
        assert!(d.observe(&healthy(0, 0, 0)).is_empty());
        assert!(d.observe(&healthy(50, 20, 40)).is_empty());
        // Freeze: delivered and hops stuck, flits in flight.
        let frozen = |c| HealthSample {
            cycle: c,
            delivered_flits: 20,
            total_hops: 40,
            injected_flits: 100,
            in_flight_flits: 80,
            router_occupancy: vec![0.95, 0.3],
            ..Default::default()
        };
        assert!(d.observe(&frozen(60)).is_empty()); // stall just began
        assert!(d.observe(&frozen(120)).is_empty()); // 60 cycles < window
        let ev = d.observe(&frozen(160)); // 100 cycles since stall start (60)
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, DiagKind::Deadlock);
        assert_eq!(ev[0].location, Location::Network);
        assert_eq!(ev[0].cycle, 160);
        // Latched: no repeat while still frozen.
        assert!(d.observe(&frozen(200)).is_empty());
    }

    #[test]
    fn deadlock_does_not_fire_when_flits_are_still_moving() {
        let mut d = DeadlockDetector::new(50);
        // Delivered stuck at 5, but hops keep rising -> that's livelock, not
        // deadlock; the deadlock detector must stay silent.
        for c in (0..500).step_by(10) {
            let s = HealthSample {
                cycle: c,
                delivered_flits: 5,
                total_hops: c, // rising
                injected_flits: 50,
                in_flight_flits: 45,
                ..Default::default()
            };
            assert!(d.observe(&s).is_empty(), "no deadlock at cycle {c}");
        }
    }

    #[test]
    fn livelock_fires_when_moving_but_not_delivering() {
        let mut l = LivelockDetector::new(100);
        let mut fired = None;
        for c in (0..400).step_by(10) {
            let s = HealthSample {
                cycle: c,
                delivered_flits: 5, // stuck
                total_hops: c * 3,  // rising -> movement
                injected_flits: 60,
                in_flight_flits: 55,
                ..Default::default()
            };
            for e in l.observe(&s) {
                assert_eq!(e.kind, DiagKind::Livelock);
                fired.get_or_insert(e.cycle);
            }
        }
        assert!(fired.is_some(), "livelock should fire");
    }

    #[test]
    fn livelock_stays_silent_on_healthy_traffic() {
        let mut l = LivelockDetector::new(50);
        for c in (0..1000).step_by(10) {
            // Both delivered and hops rising -> healthy.
            let s = healthy(c, c, c * 2);
            assert!(l.observe(&s).is_empty());
        }
    }

    #[test]
    fn congestion_fires_for_the_sustained_hotspot_only() {
        let mut d = CongestionDetector::new(100, 0.9);
        let sample = |c, occ1: f32| HealthSample {
            cycle: c,
            router_occupancy: vec![0.2, occ1, 0.5],
            ..Default::default()
        };
        // Router 1 saturated from the start.
        assert!(d.observe(&sample(0, 0.95)).is_empty());
        assert!(d.observe(&sample(60, 0.97)).is_empty());
        let ev = d.observe(&sample(120, 0.99));
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, DiagKind::Congestion);
        assert_eq!(ev[0].location, Location::Router(RouterId(1)));
        // No repeat while latched.
        assert!(d.observe(&sample(200, 0.95)).is_empty());
    }

    #[test]
    fn congestion_ignores_transient_spikes() {
        let mut d = CongestionDetector::new(100, 0.9);
        let occ = |c, hot: bool| HealthSample {
            cycle: c,
            router_occupancy: vec![if hot { 0.99 } else { 0.1 }],
            ..Default::default()
        };
        assert!(d.observe(&occ(0, true)).is_empty());
        assert!(d.observe(&occ(20, false)).is_empty()); // relieved before window
        assert!(d.observe(&occ(200, false)).is_empty());
    }

    #[test]
    fn starvation_fires_for_a_persistently_ungranted_flow() {
        let mut d = StarvationDetector::new(100);
        let s = |c, granted2: bool| HealthSample {
            cycle: c,
            flow_pending: vec![true, true, true],
            flow_granted: vec![true, true, granted2],
            ..Default::default()
        };
        // Flow 2 pending but never granted; flows 0,1 fine.
        assert!(d.observe(&s(0, false)).is_empty());
        assert!(d.observe(&s(60, false)).is_empty());
        let ev = d.observe(&s(120, false));
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, DiagKind::Starvation);
        assert_eq!(ev[0].location, Location::Flow(2));
    }

    #[test]
    fn starvation_resets_when_the_flow_is_granted() {
        let mut d = StarvationDetector::new(100);
        let s = |c, granted: bool| HealthSample {
            cycle: c,
            flow_pending: vec![true],
            flow_granted: vec![granted],
            ..Default::default()
        };
        assert!(d.observe(&s(0, false)).is_empty());
        assert!(d.observe(&s(60, true)).is_empty()); // granted -> reset
        assert!(d.observe(&s(120, false)).is_empty()); // window restarts
        assert!(d.observe(&s(150, false)).is_empty()); // < window since reset
    }

    #[test]
    fn invariants_accept_a_conserving_run_and_flag_loss() {
        let good = HealthSample {
            cycle: 10,
            injected_flits: 100,
            delivered_flits: 70,
            dropped_flits: 5,
            in_flight_flits: 25,
            ..Default::default()
        };
        assert!(InvariantChecker::check_flit_conservation(&good).is_ok());

        let lossy = HealthSample {
            in_flight_flits: 20, // 70 + 5 + 20 = 95 != 100 -> 5 lost
            ..good.clone()
        };
        assert!(matches!(
            InvariantChecker::check_flit_conservation(&lossy),
            Err(Violation::FlitConservation { .. })
        ));
    }

    #[test]
    fn invariant_checker_flags_non_monotonic_counters() {
        let mut ic = InvariantChecker::new();
        let mk = |c, delivered| HealthSample {
            cycle: c,
            injected_flits: 100,
            delivered_flits: delivered,
            in_flight_flits: 100 - delivered,
            ..Default::default()
        };
        assert!(ic.observe(&mk(0, 10)).is_empty());
        assert!(ic.observe(&mk(10, 20)).is_empty());
        // Delivered goes backwards.
        let v = ic.observe(&mk(20, 15));
        assert!(v.iter().any(|x| matches!(
            x,
            Violation::NonMonotonic {
                counter: "delivered_flits",
                ..
            }
        )));
    }
}
