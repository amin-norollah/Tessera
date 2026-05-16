//! `tessera-power` — native, transparent power & area models (P3-F1/F2/F3/F5).
//!
//! These are Tessera's own Rust formulas, driven entirely by the cited
//! [`ParamDb`] (INITIAL_PRD §9A "native analytical" fidelity mode): fast,
//! editable, and citable. The validated external references (DSENT/ORION/McPAT)
//! sit behind the same [`PowerModel`]/[`AreaModel`] traits as FFI `*-sys` crates
//! (Phase 3, step 6) so a run just names the model + parameter set.
//!
//! # What is modelled here
//! - **Dynamic energy** (P3-F1): `E = α·C·V²` charged per datapath [`Activity`].
//! - **Leakage** (P3-F2): `P_leak = I_leak(T,V)·V` under two selectable
//!   [`LeakageLaw`]s — classic superlinear subthreshold and TEI-aware.
//! - **Link energy** (P3-F3): distance-dependent wire + repeater energy.
//! - **Area** (P3-F5): router area from buffer/crossbar/control coefficients.
//!
//! # Accuracy stance
//! The native model trades some absolute accuracy for transparency/speed/
//! editability (§9A trade-off); the shipped parameter values are representative
//! defaults. Absolute accuracy against DSENT/HotSpot is a later deliverable via
//! FFI + *calibrated mode* (P3-F10) and the regression gate (G3.1). The tests in
//! this crate therefore assert model **behaviour** (V² scaling, leakage-law
//! shapes, link distance scaling, area monotonicity), not absolute numbers.

use tessera_core::{ActivityCounts, RouterId};
use tessera_params::ParamDb;

/// Energy in joules.
pub type Joules = f64;
/// Power in watts.
pub type Watts = f64;
/// Area in square millimetres.
pub type Mm2 = f64;

/// Boltzmann constant (J/K).
const K_BOLTZMANN: f64 = 1.380_649e-23;
/// Elementary charge (C).
const Q_ELECTRON: f64 = 1.602_176_634e-19;
/// Celsius-to-Kelvin offset.
const KELVIN: f64 = 273.15;

/// A charged datapath event (INITIAL_PRD §20.9). Emitted by the existing stage
/// kernels; the power model turns each into energy. `LinkBits(n)` carries the
/// bit count of a single link traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    BufWrite,
    BufRead,
    Xbar,
    VaArb,
    SaArb,
    LinkBits(u32),
    Clock,
}

/// Structural parameters of a router the area model needs. Defined here (not in
/// `tessera-model`) because the §20 signatures are indicative, not frozen, and
/// this keeps `tessera-power` dependent only on `tessera-core` + the param DB.
#[derive(Clone, Copy, Debug)]
pub struct RouterCfg {
    pub ports: u8,
    pub num_vcs: u8,
    pub buffer_depth: u16,
    pub flit_width_bits: u32,
}

/// Which leakage law the native model uses (P3-F2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LeakageLaw {
    /// Classic superlinear subthreshold: leakage rises monotonically with
    /// temperature (the usual "hotter ⇒ leakier" behaviour).
    #[default]
    ClassicSubthreshold,
    /// TEI-aware: at supply voltages below `leakage.tei_v_inv` the
    /// temperature/leakage slope inverts (D5), important for near-threshold,
    /// sub-90 nm studies.
    TeiAware,
}

/// Computes dynamic energy per event and leakage power per router.
pub trait PowerModel: Send + Sync {
    /// Energy charged for one `activity` event at router `node`.
    fn dynamic_energy(&self, activity: Activity, node: RouterId, p: &ParamDb) -> Joules;
    /// Leakage power of router `node` at temperature `temp_c`.
    fn leakage_power(&self, node: RouterId, temp_c: f32, p: &ParamDb) -> Watts;
}

/// Computes router silicon area.
pub trait AreaModel: Send + Sync {
    fn router_area(&self, cfg: &RouterCfg, p: &ParamDb) -> Mm2;
}

/// The native analytical power model (§9A). Homogeneous across routers in this
/// phase; `node` is accepted for future per-router heterogeneity.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativePowerModel {
    pub leakage_law: LeakageLaw,
}

impl NativePowerModel {
    pub fn new(leakage_law: LeakageLaw) -> Self {
        Self { leakage_law }
    }

    /// Distance-dependent link energy (P3-F3): `(α·C_wire·V² +
    /// E_repeater)·bits·hop_mm`. Wire capacitance contributes the `V²` term;
    /// repeater energy is already an energy-per-bit-per-mm. Exposed directly
    /// (beyond the trait) so links of non-default length can be charged.
    pub fn link_energy(&self, bits: u32, hop_mm: f64, p: &ParamDb) -> Joules {
        let v = p.electrical.voltage.value;
        let alpha = p.power.alpha.value;
        let wire = alpha * p.link.wire_cap_per_mm.value * v * v;
        let per_bit_per_mm = wire + p.link.repeater_e_per_bit_per_mm.value;
        per_bit_per_mm * bits as f64 * hop_mm
    }

    /// Leakage current (A) for the selected law at `temp_c`.
    pub fn leakage_current(&self, temp_c: f64, p: &ParamDb) -> f64 {
        let i0 = p.leakage.i_leak0.value;
        let t_ref_c = p.leakage.t_ref_c.value;
        match self.leakage_law {
            LeakageLaw::ClassicSubthreshold => {
                // Physics-form superlinear subthreshold current:
                //   I(T) = I0 · (T/Tref)² · exp( (q·Vth)/(η·k) · (1/Tref − 1/T) )
                // Both factors rise with T ⇒ monotonically increasing; equals
                // I0 exactly at Tref.
                let t = temp_c + KELVIN;
                let t_ref = t_ref_c + KELVIN;
                let vth = p.leakage.vth.value;
                let eta = p.leakage.eta.value;
                let a = (Q_ELECTRON * vth) / (eta * K_BOLTZMANN);
                i0 * (t / t_ref).powi(2) * (a * (1.0 / t_ref - 1.0 / t)).exp()
            }
            LeakageLaw::TeiAware => {
                // I(T) = I0 · exp( β·(T − Tref) ),  β = tei_coeff·(V − V_inv).
                // β>0 above V_inv (normal), β<0 below it (inversion, D5).
                let v = p.electrical.voltage.value;
                let beta = p.leakage.tei_coeff.value * (v - p.leakage.tei_v_inv.value);
                i0 * (beta * (temp_c - t_ref_c)).exp()
            }
        }
    }
}

/// Dynamic energy attributed to each router subsystem over some interval, in
/// joules — the output of [`NativePowerModel::energy_from_counts`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EnergyBreakdown {
    /// Buffer read + write energy.
    pub buffers: Joules,
    /// Crossbar traversal energy.
    pub crossbar: Joules,
    /// VC + switch arbitration energy.
    pub arbitration: Joules,
    /// Link (wire + repeater) energy.
    pub links: Joules,
    /// Clock-tree energy.
    pub clock: Joules,
}

impl EnergyBreakdown {
    /// Total dynamic energy across all subsystems.
    pub fn total(&self) -> Joules {
        self.buffers + self.crossbar + self.arbitration + self.links + self.clock
    }

    /// Accumulate another breakdown (network-wide reduction, fixed order).
    pub fn add(&mut self, o: &EnergyBreakdown) {
        self.buffers += o.buffers;
        self.crossbar += o.crossbar;
        self.arbitration += o.arbitration;
        self.links += o.links;
        self.clock += o.clock;
    }
}

impl NativePowerModel {
    /// Convert accumulated [`ActivityCounts`] into a per-subsystem dynamic
    /// [`EnergyBreakdown`] (P3-F1). This is the off-the-hot-path pass: the
    /// kernel only counts events; energy is computed here from the parameter DB.
    /// `link_flits` are charged at the profile's flit width over one tile pitch.
    pub fn energy_from_counts(
        &self,
        counts: &ActivityCounts,
        node: RouterId,
        p: &ParamDb,
    ) -> EnergyBreakdown {
        let e = |a| self.dynamic_energy(a, node, p);
        let link_flit_energy = self.link_energy(p.flit_width_bits, p.link.tile_pitch_mm.value, p);
        EnergyBreakdown {
            buffers: counts.buf_write as f64 * e(Activity::BufWrite)
                + counts.buf_read as f64 * e(Activity::BufRead),
            crossbar: counts.xbar as f64 * e(Activity::Xbar),
            arbitration: counts.va_arb as f64 * e(Activity::VaArb)
                + counts.sa_arb as f64 * e(Activity::SaArb),
            links: counts.link_flits as f64 * link_flit_energy,
            clock: counts.clock as f64 * e(Activity::Clock),
        }
    }

    /// Average power (W) implied by `energy_j` dissipated over `cycles` cycles at
    /// the profile clock: `P = E · f / cycles`. Returns 0 for a zero-cycle span.
    pub fn average_power(&self, energy_j: Joules, cycles: u64, p: &ParamDb) -> Watts {
        if cycles == 0 {
            0.0
        } else {
            energy_j * p.electrical.frequency.value / cycles as f64
        }
    }
}

impl PowerModel for NativePowerModel {
    fn dynamic_energy(&self, activity: Activity, _node: RouterId, p: &ParamDb) -> Joules {
        let v = p.electrical.voltage.value;
        let alpha = p.power.alpha.value;
        let cv2 = |c: f64| alpha * c * v * v;
        match activity {
            Activity::BufWrite => cv2(p.power.c_buf_write.value),
            Activity::BufRead => cv2(p.power.c_buf_read.value),
            Activity::Xbar => cv2(p.power.c_xbar.value),
            Activity::VaArb => cv2(p.power.c_va_arb.value),
            Activity::SaArb => cv2(p.power.c_sa_arb.value),
            Activity::Clock => cv2(p.power.c_clock.value),
            Activity::LinkBits(bits) => self.link_energy(bits, p.link.tile_pitch_mm.value, p),
        }
    }

    fn leakage_power(&self, _node: RouterId, temp_c: f32, p: &ParamDb) -> Watts {
        self.leakage_current(temp_c as f64, p) * p.electrical.voltage.value
    }
}

/// The native analytical area model (P3-F5): additive buffer + crossbar +
/// control area from the DB coefficients.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeAreaModel;

impl AreaModel for NativeAreaModel {
    fn router_area(&self, cfg: &RouterCfg, p: &ParamDb) -> Mm2 {
        let bits = cfg.flit_width_bits as f64;
        let ports = cfg.ports as f64;
        let vcs = cfg.num_vcs as f64;
        let depth = cfg.buffer_depth as f64;
        // Input buffers: one FIFO per (port, VC), `depth` flits each.
        let buffers = p.area.a_buf_bit.value * bits * depth * vcs * ports;
        // Crossbar grows with the square of the port count and flit width.
        let crossbar = p.area.a_xbar_coeff.value * ports * ports * bits;
        buffers + crossbar + p.area.a_arb.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::ActivityCounts;
    use tessera_params::ParamDb;

    const PTM: &str = include_str!("../../../params/ptm_22nm.toml");

    fn db() -> ParamDb {
        ParamDb::from_toml(PTM).unwrap()
    }

    const R0: RouterId = RouterId(0);

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() <= b.abs() * 1e-9 + 1e-18, "{a} != {b}");
    }

    #[test]
    fn dynamic_energy_matches_alpha_c_v_squared() {
        let m = NativePowerModel::default();
        let p = db();
        let (alpha, v) = (p.power.alpha.value, p.electrical.voltage.value);
        approx(
            m.dynamic_energy(Activity::BufWrite, R0, &p),
            alpha * p.power.c_buf_write.value * v * v,
        );
        approx(
            m.dynamic_energy(Activity::Clock, R0, &p),
            alpha * p.power.c_clock.value * v * v,
        );
    }

    #[test]
    fn dynamic_energy_scales_with_voltage_squared() {
        let m = NativePowerModel::default();
        let mut p = db();
        let base = m.dynamic_energy(Activity::Xbar, R0, &p);
        p.electrical.voltage.value *= 2.0;
        let doubled = m.dynamic_energy(Activity::Xbar, R0, &p);
        approx(doubled, base * 4.0);
    }

    #[test]
    fn known_activity_sequence_sums() {
        let m = NativePowerModel::default();
        let p = db();
        // One head flit through one router: write, route/read, VA, SA, xbar.
        let seq = [
            Activity::BufWrite,
            Activity::BufRead,
            Activity::VaArb,
            Activity::SaArb,
            Activity::Xbar,
        ];
        let total: f64 = seq.iter().map(|&a| m.dynamic_energy(a, R0, &p)).sum();
        let expect = m.dynamic_energy(Activity::BufWrite, R0, &p)
            + m.dynamic_energy(Activity::BufRead, R0, &p)
            + m.dynamic_energy(Activity::VaArb, R0, &p)
            + m.dynamic_energy(Activity::SaArb, R0, &p)
            + m.dynamic_energy(Activity::Xbar, R0, &p);
        approx(total, expect);
        assert!(total > 0.0);
    }

    #[test]
    fn link_energy_scales_linearly_in_bits_and_distance() {
        let m = NativePowerModel::default();
        let p = db();
        let e1 = m.link_energy(32, 1.0, &p);
        let e2 = m.link_energy(64, 1.0, &p);
        let e3 = m.link_energy(32, 3.0, &p);
        approx(e2, e1 * 2.0); // double the bits
        approx(e3, e1 * 3.0); // triple the distance
        assert!(e1 > 0.0);
    }

    #[test]
    fn link_activity_uses_tile_pitch_as_hop_distance() {
        let m = NativePowerModel::default();
        let p = db();
        let via_activity = m.dynamic_energy(Activity::LinkBits(32), R0, &p);
        let via_helper = m.link_energy(32, p.link.tile_pitch_mm.value, &p);
        approx(via_activity, via_helper);
    }

    #[test]
    fn leakage_equals_i0_times_v_at_reference_temperature() {
        let p = db();
        let v = p.electrical.voltage.value;
        let t_ref = p.leakage.t_ref_c.value as f32;
        for law in [LeakageLaw::ClassicSubthreshold, LeakageLaw::TeiAware] {
            let m = NativePowerModel::new(law);
            approx(m.leakage_power(R0, t_ref, &p), p.leakage.i_leak0.value * v);
        }
    }

    #[test]
    fn classic_leakage_rises_monotonically_with_temperature() {
        let m = NativePowerModel::new(LeakageLaw::ClassicSubthreshold);
        let p = db();
        let cold = m.leakage_power(R0, 25.0, &p);
        let warm = m.leakage_power(R0, 60.0, &p);
        let hot = m.leakage_power(R0, 100.0, &p);
        assert!(cold < warm && warm < hot, "{cold} < {warm} < {hot}");
    }

    #[test]
    fn tei_inverts_slope_below_inversion_voltage() {
        let m = NativePowerModel::new(LeakageLaw::TeiAware);
        let mut p = db();

        // Above V_inv: leakage rises with temperature (normal direction).
        p.electrical.voltage.value = p.leakage.tei_v_inv.value + 0.2;
        assert!(m.leakage_power(R0, 100.0, &p) > m.leakage_power(R0, 25.0, &p));

        // Below V_inv: the slope inverts — hotter is *less* leaky (D5).
        p.electrical.voltage.value = p.leakage.tei_v_inv.value - 0.2;
        assert!(m.leakage_power(R0, 100.0, &p) < m.leakage_power(R0, 25.0, &p));
    }

    #[test]
    fn area_is_positive_and_monotonic_in_each_dimension() {
        let a = NativeAreaModel;
        let p = db();
        let base = RouterCfg {
            ports: 5,
            num_vcs: 2,
            buffer_depth: 4,
            flit_width_bits: 32,
        };
        let base_area = a.router_area(&base, &p);
        assert!(base_area > 0.0);

        let more_ports = a.router_area(&RouterCfg { ports: 7, ..base }, &p);
        let more_vcs = a.router_area(&RouterCfg { num_vcs: 4, ..base }, &p);
        let deeper = a.router_area(
            &RouterCfg {
                buffer_depth: 8,
                ..base
            },
            &p,
        );
        let wider = a.router_area(
            &RouterCfg {
                flit_width_bits: 64,
                ..base
            },
            &p,
        );

        assert!(more_ports > base_area);
        assert!(more_vcs > base_area);
        assert!(deeper > base_area);
        assert!(wider > base_area);
    }

    #[test]
    fn energy_from_counts_matches_manual_sum() {
        let m = NativePowerModel::default();
        let p = db();
        let c = ActivityCounts {
            buf_write: 10,
            buf_read: 8,
            xbar: 5,
            va_arb: 3,
            sa_arb: 5,
            link_flits: 5,
            clock: 100,
        };
        let b = m.energy_from_counts(&c, R0, &p);
        let e = |a| m.dynamic_energy(a, R0, &p);
        approx(
            b.buffers,
            10.0 * e(Activity::BufWrite) + 8.0 * e(Activity::BufRead),
        );
        approx(b.crossbar, 5.0 * e(Activity::Xbar));
        approx(
            b.arbitration,
            3.0 * e(Activity::VaArb) + 5.0 * e(Activity::SaArb),
        );
        approx(
            b.links,
            5.0 * m.link_energy(p.flit_width_bits, p.link.tile_pitch_mm.value, &p),
        );
        approx(b.clock, 100.0 * e(Activity::Clock));
        approx(
            b.total(),
            b.buffers + b.crossbar + b.arbitration + b.links + b.clock,
        );
        assert!(b.total() > 0.0);
    }

    #[test]
    fn zero_counts_yield_zero_energy() {
        let m = NativePowerModel::default();
        let p = db();
        let b = m.energy_from_counts(&ActivityCounts::default(), R0, &p);
        assert_eq!(b.total(), 0.0);
    }

    #[test]
    fn average_power_scales_with_frequency_and_span() {
        let m = NativePowerModel::default();
        let p = db();
        let energy = 1.0e-9;
        let f = p.electrical.frequency.value;
        approx(m.average_power(energy, 1000, &p), energy * f / 1000.0);
        assert_eq!(m.average_power(energy, 0, &p), 0.0);
    }

    #[test]
    fn energy_breakdown_add_reduces_in_order() {
        let mut acc = EnergyBreakdown::default();
        let one = EnergyBreakdown {
            buffers: 1.0,
            crossbar: 2.0,
            arbitration: 3.0,
            links: 4.0,
            clock: 5.0,
        };
        acc.add(&one);
        acc.add(&one);
        approx(acc.total(), 2.0 * one.total());
        approx(acc.buffers, 2.0);
    }
}
