//! TEI feedback loop + RTM thermal policies (P3-F12/F13).
//!
//! The Temperature-Effect-Inversion feedback loop (D5) closes the ring between
//! power and temperature: on a configurable interval (default 100k cycles,
//! matching PAT-Noxim) it recomputes each router's power as
//! `scale · (dynamic + leakage(T))`, re-solves the [`ThermalModel`], and lets an
//! RTM [`ThermalPolicy`] read the new temperatures and set the next interval's
//! per-router `scale`. Because leakage depends on temperature, temperature and
//! power are mutually coupled: the loop iterates them to a fixed point.
//!
//! Policies are **pure functions of observable state** (temperatures here;
//! occupancy/leakage can be added), never baked into the datapath (P3-F13): the
//! same seam a WASM policy plugin will plug into later. [`ThresholdThrottle`] is
//! the shipped example: above a trip temperature it scales a router's power down
//! (clock-/power-gating), demonstrably bending the thermal trajectory (G3.3).

use tessera_core::{ActivityCounts, RouterId};
use tessera_params::ParamDb;
use tessera_power::NativePowerModel;

use crate::{Celsius, ThermalModel, Watts};

/// Cadence of the feedback loop.
#[derive(Clone, Copy, Debug)]
pub struct TeiConfig {
    /// Cycles between thermal re-solves (default 100_000, per PAT-Noxim).
    pub interval_cycles: u64,
    /// Wall-clock seconds advanced per interval (interval_cycles / clock).
    pub dt_s: f32,
}

impl TeiConfig {
    /// Derive the interval's wall-clock duration from the profile clock.
    pub fn from_interval(interval_cycles: u64, p: &ParamDb) -> Self {
        let dt_s = (interval_cycles as f64 / p.electrical.frequency.value) as f32;
        Self {
            interval_cycles,
            dt_s,
        }
    }
}

impl Default for TeiConfig {
    fn default() -> Self {
        Self {
            interval_cycles: 100_000,
            dt_s: 1e-4,
        }
    }
}

/// A run-time-management policy: reads observable per-router temperatures and
/// returns a per-router power `scale` in `0.0..=1.0` (1 = full power, `<1` =
/// throttled/gated) applied on the next interval (P3-F13).
pub trait ThermalPolicy: Send + Sync {
    fn scale(&mut self, temps: &[Celsius]) -> Vec<f32>;
}

/// The trivial policy: never throttles.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoThrottle;

impl ThermalPolicy for NoThrottle {
    fn scale(&mut self, temps: &[Celsius]) -> Vec<f32> {
        vec![1.0; temps.len()]
    }
}

/// Throttle any router at/above `trip_c` to `scale` of its power; others run
/// full-speed. A minimal thermal-cap RTM policy (G3.3).
#[derive(Clone, Copy, Debug)]
pub struct ThresholdThrottle {
    pub trip_c: Celsius,
    pub scale: f32,
}

impl ThermalPolicy for ThresholdThrottle {
    fn scale(&mut self, temps: &[Celsius]) -> Vec<f32> {
        temps
            .iter()
            .map(|&t| if t >= self.trip_c { self.scale } else { 1.0 })
            .collect()
    }
}

/// The TEI feedback loop (P3-F12) over a [`ThermalModel`] and a
/// [`ThermalPolicy`], driven by the native power model.
pub struct TeiLoop<T: ThermalModel, Pol: ThermalPolicy> {
    power: NativePowerModel,
    thermal: T,
    policy: Pol,
    cfg: TeiConfig,
    temps: Vec<Celsius>,
    scale: Vec<f32>,
    power_w: Vec<Watts>,
}

impl<T: ThermalModel, Pol: ThermalPolicy> TeiLoop<T, Pol> {
    pub fn new(power: NativePowerModel, thermal: T, policy: Pol, cfg: TeiConfig) -> Self {
        let temps = thermal.temperatures().to_vec();
        let n = temps.len();
        Self {
            power,
            thermal,
            policy,
            cfg,
            temps,
            scale: vec![1.0; n],
            power_w: vec![0.0; n],
        }
    }

    /// Advance one interval: recompute per-router power from the interval's
    /// activity counts plus temperature-dependent leakage (scaled by the current
    /// RTM decision), re-solve the thermal network, then let the policy set the
    /// next interval's scale from the new temperatures.
    pub fn advance(&mut self, counts: &[ActivityCounts], p: &ParamDb) -> &[Celsius] {
        assert_eq!(
            counts.len(),
            self.temps.len(),
            "one activity-count entry per tile"
        );
        for (r, c) in counts.iter().enumerate() {
            let node = RouterId(r as u32);
            let dyn_e = self.power.energy_from_counts(c, node, p).total();
            let dyn_p = self.power.average_power(dyn_e, self.cfg.interval_cycles, p);
            let leak_p =
                self.power.leakage_current(self.temps[r] as f64, p) * p.electrical.voltage.value;
            self.power_w[r] = self.scale[r] as f64 * (dyn_p + leak_p);
        }
        self.temps = self.thermal.step(&self.power_w, self.cfg.dt_s).to_vec();
        self.scale = self.policy.scale(&self.temps);
        &self.temps
    }

    /// The most recent per-router temperatures.
    pub fn temperatures(&self) -> &[Celsius] {
        &self.temps
    }

    /// The per-router power scale the policy will apply next interval.
    pub fn scale(&self) -> &[f32] {
        &self.scale
    }

    /// The per-router power (W) charged in the last [`Self::advance`].
    pub fn power_w(&self) -> &[Watts] {
        &self.power_w
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RcGrid;
    use tessera_power::LeakageLaw;

    const PTM: &str = include_str!("../../../params/ptm_22nm.toml");

    /// A profile with leakage raised enough to move temperature visibly in f32
    /// while keeping cooling (`g_vert = 1/r_conv`) strong enough that the
    /// temperature/leakage feedback settles to a *stable* fixed point rather than
    /// running away: for exercising the loop mechanism, not absolute accuracy.
    fn hot_db() -> ParamDb {
        let mut p = ParamDb::from_toml(PTM).unwrap();
        p.leakage.i_leak0.value = 0.5; // ~0.7 W leakage/router near Tref
        p.thermal.r_conv_k_per_w.value = 1.0; // g_vert = 1.0 W/K (stable)
        p
    }

    fn steady_counts(n: usize) -> Vec<ActivityCounts> {
        vec![
            ActivityCounts {
                buf_write: 100,
                buf_read: 100,
                xbar: 100,
                va_arb: 50,
                sa_arb: 100,
                link_flits: 80,
                clock: 1000,
            };
            n
        ]
    }

    fn max_temp(t: &[Celsius]) -> Celsius {
        t.iter().copied().fold(f32::MIN, f32::max)
    }

    #[test]
    fn no_throttle_is_identity() {
        let mut pol = NoThrottle;
        assert_eq!(pol.scale(&[10.0, 90.0, 200.0]), vec![1.0, 1.0, 1.0]);
    }

    #[test]
    fn threshold_throttle_trips_per_router() {
        let mut pol = ThresholdThrottle {
            trip_c: 50.0,
            scale: 0.25,
        };
        assert_eq!(pol.scale(&[49.9, 50.0, 80.0]), vec![1.0, 0.25, 0.25]);
    }

    #[test]
    fn loop_converges_and_heats_above_ambient() {
        let p = hot_db();
        let amb = p.thermal.t_amb_c.value as f32;
        let grid = RcGrid::from_params(2, 2, &p, true);
        let power = NativePowerModel::new(LeakageLaw::ClassicSubthreshold);
        let mut tei = TeiLoop::new(power, grid, NoThrottle, TeiConfig::default());
        let counts = steady_counts(4);

        let mut last = tei.temperatures().to_vec();
        for _ in 0..600 {
            last = tei.advance(&counts, &p).to_vec();
        }
        assert!(last.iter().all(|&t| t.is_finite()));
        assert!(max_temp(&last) > amb, "die should heat: {last:?}");

        // Converged: another interval barely moves it.
        let next = tei.advance(&counts, &p).to_vec();
        assert!((max_temp(&next) - max_temp(&last)).abs() < 0.05);
    }

    #[test]
    fn throttling_lowers_steady_temperature() {
        // G3.3: an RTM policy demonstrably bends the thermal trajectory.
        let p = hot_db();
        let amb = p.thermal.t_amb_c.value as f32;
        let counts = steady_counts(4);
        let power = || NativePowerModel::new(LeakageLaw::ClassicSubthreshold);

        let mut free = TeiLoop::new(
            power(),
            RcGrid::from_params(2, 2, &p, true),
            NoThrottle,
            TeiConfig::default(),
        );
        let trip = amb + 0.3;
        let mut throttled = TeiLoop::new(
            power(),
            RcGrid::from_params(2, 2, &p, true),
            ThresholdThrottle {
                trip_c: trip,
                scale: 0.1,
            },
            TeiConfig::default(),
        );

        let mut free_t = 0.0;
        let mut thr_t = 0.0;
        let mut engaged = false;
        for _ in 0..600 {
            free_t = max_temp(free.advance(&counts, &p));
            thr_t = max_temp(throttled.advance(&counts, &p));
            engaged |= throttled.scale().iter().any(|&s| s < 1.0);
        }
        assert!(
            free_t > trip,
            "unthrottled must exceed the trip: {free_t} vs {trip}"
        );
        assert!(
            thr_t < free_t,
            "throttling should lower steady temp: throttled {thr_t} vs free {free_t}"
        );
        assert!(
            engaged,
            "the throttle policy should have engaged under thermal stress"
        );
    }

    #[test]
    fn tei_leakage_feedback_raises_temperature_vs_frozen_leakage() {
        // With the classic law, hotter => leakier => hotter: the coupled loop
        // must settle above what a single leakage-at-ambient estimate predicts.
        let p = hot_db();
        let power = NativePowerModel::new(LeakageLaw::ClassicSubthreshold);
        let grid = RcGrid::from_params(1, 1, &p, false);
        let mut tei = TeiLoop::new(power, grid, NoThrottle, TeiConfig::default());
        let counts = steady_counts(1);

        let mut settled = 0.0;
        for _ in 0..800 {
            settled = tei.advance(&counts, &p)[0];
        }

        // One-shot estimate using leakage frozen at ambient (no feedback).
        let amb = p.thermal.t_amb_c.value;
        let leak_amb = power.leakage_current(amb, &p) * p.electrical.voltage.value;
        let g_vert = 1.0 / p.thermal.r_conv_k_per_w.value;
        let frozen_estimate = amb + leak_amb / g_vert;

        assert!(
            settled as f64 > frozen_estimate,
            "feedback temp {settled} should exceed frozen-leakage estimate {frozen_estimate}"
        );
    }
}
