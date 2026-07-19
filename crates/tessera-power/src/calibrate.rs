//! Calibrated mode (P3-F10): fit the native model to reference measurements.
//!
//! The native dynamic-energy model is **linear in its energy coefficients**:
//! every per-event capacitance and the two link-energy coefficients enter the
//! total as a simple product with the activity counts. So a single
//! multiplicative correction `k` applied uniformly to all of them scales the
//! predicted dynamic energy by exactly `k` at every operating point. That makes
//! the best-fit `k` a **closed-form least-squares** solution against a set of
//! reference points (from a DSENT/ORION FFI run, silicon, or a published table):
//!
//! ```text
//! k* = Σ (model_i · target_i) / Σ (model_i²)
//! ```
//!
//! The fit is applied by scaling the coefficients in a cloned [`ParamDb`],
//! producing a new **citable profile** whose provenance records the calibration
//! (P3-F10: "persist as a new citable profile"). The result round-trips through
//! TOML like any other profile.

use tessera_core::{ActivityCounts, RouterId};
use tessera_params::ParamDb;

use crate::{Joules, NativePowerModel};

/// One reference operating point: an activity fingerprint and the dynamic energy
/// a trusted reference reports for it.
#[derive(Clone, Copy, Debug)]
pub struct DynPoint {
    pub counts: ActivityCounts,
    /// Reference dynamic energy for this activity (J).
    pub target_energy_j: Joules,
}

impl DynPoint {
    pub fn new(counts: ActivityCounts, target_energy_j: Joules) -> Self {
        Self {
            counts,
            target_energy_j,
        }
    }
}

/// Outcome of a calibration fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalibrationReport {
    /// The best-fit multiplicative correction applied to the energy coefficients.
    pub scale: f64,
    /// RMS error (J) of the uncalibrated model over the reference points.
    pub rms_before: f64,
    /// RMS error (J) after applying `scale`.
    pub rms_after: f64,
    pub n_points: usize,
}

impl NativePowerModel {
    /// Closed-form least-squares fit of a single energy-coefficient correction
    /// `k` so the native dynamic energy best matches the reference `points`.
    /// Returns `None` if there are no points or every model prediction is zero
    /// (nothing to fit against).
    pub fn fit_dynamic_scale(&self, points: &[DynPoint], p: &ParamDb) -> Option<CalibrationReport> {
        if points.is_empty() {
            return None;
        }
        let node = RouterId(0);
        let (mut num, mut den, mut sq_before) = (0.0, 0.0, 0.0);
        for pt in points {
            let model = self.energy_from_counts(&pt.counts, node, p).total();
            num += model * pt.target_energy_j;
            den += model * model;
            let e = model - pt.target_energy_j;
            sq_before += e * e;
        }
        if den == 0.0 {
            return None;
        }
        let scale = num / den;

        let mut sq_after = 0.0;
        for pt in points {
            let model = self.energy_from_counts(&pt.counts, node, p).total();
            let e = scale * model - pt.target_energy_j;
            sq_after += e * e;
        }
        let n = points.len() as f64;
        Some(CalibrationReport {
            scale,
            rms_before: (sq_before / n).sqrt(),
            rms_after: (sq_after / n).sqrt(),
            n_points: points.len(),
        })
    }
}

/// Produce a calibrated profile: clone `base`, scale every dynamic-energy
/// coefficient (per-event capacitances + link wire/repeater energy) by `scale`,
/// and record the calibration in each affected parameter's provenance so the fit
/// is self-documenting (P3-F10). Leakage, area, and thermal params are untouched
///: this fit calibrates dynamic energy only.
pub fn apply_dynamic_scale(base: &ParamDb, scale: f64, reference_label: &str) -> ParamDb {
    let mut db = base.clone();
    db.profile = format!("{} (calibrated)", base.profile);

    let base_profile = base.profile.clone();
    let recalibrate = |param: &mut tessera_params::Param<f64>| {
        param.value *= scale;
        param.source.label = format!(
            "{} [calibrated x{scale:.4} vs {reference_label}]",
            param.source.label
        );
        param.notes = if param.notes.is_empty() {
            format!("calibrated x{scale:.4} from '{base_profile}'")
        } else {
            format!(
                "{} | calibrated x{scale:.4} from '{base_profile}'",
                param.notes
            )
        };
    };

    recalibrate(&mut db.power.c_buf_write);
    recalibrate(&mut db.power.c_buf_read);
    recalibrate(&mut db.power.c_xbar);
    recalibrate(&mut db.power.c_va_arb);
    recalibrate(&mut db.power.c_sa_arb);
    recalibrate(&mut db.power.c_clock);
    recalibrate(&mut db.link.wire_cap_per_mm);
    recalibrate(&mut db.link.repeater_e_per_bit_per_mm);

    db
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_params::Strictness;

    const PTM: &str = include_str!("../../../params/ptm_22nm.toml");

    fn db() -> ParamDb {
        ParamDb::from_toml(PTM).unwrap()
    }

    fn counts(scale: u64) -> ActivityCounts {
        ActivityCounts {
            buf_write: 10 * scale,
            buf_read: 8 * scale,
            xbar: 5 * scale,
            va_arb: 3 * scale,
            sa_arb: 5 * scale,
            link_flits: 7 * scale,
            clock: 100 * scale,
        }
    }

    #[test]
    fn fit_recovers_a_known_scale_exactly() {
        let m = NativePowerModel::default();
        let p = db();
        let node = RouterId(0);
        let true_k = 1.7;
        // Targets are the native prediction times a known factor => perfect fit.
        let points: Vec<DynPoint> = (1..=5)
            .map(|s| {
                let c = counts(s);
                let target = true_k * m.energy_from_counts(&c, node, &p).total();
                DynPoint::new(c, target)
            })
            .collect();

        let report = m.fit_dynamic_scale(&points, &p).unwrap();
        assert!(
            (report.scale - true_k).abs() < 1e-9,
            "scale {}",
            report.scale
        );
        assert!(report.rms_after < report.rms_before);
        assert!(
            report.rms_after < 1e-18,
            "should fit exactly: {}",
            report.rms_after
        );
        assert_eq!(report.n_points, 5);
    }

    #[test]
    fn fit_reduces_error_on_noisy_targets() {
        let m = NativePowerModel::default();
        let p = db();
        let node = RouterId(0);
        // Deterministic pseudo-noise around a 2.0x offset.
        let points: Vec<DynPoint> = (1..=6)
            .map(|s| {
                let c = counts(s);
                let base = m.energy_from_counts(&c, node, &p).total();
                let noise = if s % 2 == 0 { 1.05 } else { 0.95 };
                DynPoint::new(c, 2.0 * base * noise)
            })
            .collect();

        let report = m.fit_dynamic_scale(&points, &p).unwrap();
        assert!(
            report.scale > 1.8 && report.scale < 2.2,
            "scale {}",
            report.scale
        );
        assert!(report.rms_after <= report.rms_before);
    }

    #[test]
    fn applying_the_scale_makes_energy_match_the_factor() {
        let m = NativePowerModel::default();
        let base = db();
        let node = RouterId(0);
        let k = 1.7;
        let calibrated = apply_dynamic_scale(&base, k, "DSENT 22nm");

        // Every operating point's energy is scaled by exactly k.
        for s in 1..=4 {
            let c = counts(s);
            let before = m.energy_from_counts(&c, node, &base).total();
            let after = m.energy_from_counts(&c, node, &calibrated).total();
            assert!(
                (after - k * before).abs() <= (k * before).abs() * 1e-9,
                "after {after} != k*before {}",
                k * before
            );
        }
    }

    #[test]
    fn calibrated_profile_is_renamed_cited_and_still_valid() {
        let base = db();
        let calibrated = apply_dynamic_scale(&base, 1.3, "ORION 3.0");
        assert_eq!(calibrated.profile, "PTM 22nm LVT (calibrated)");
        // Provenance records the calibration.
        assert!(calibrated
            .power
            .c_xbar
            .source
            .label
            .contains("calibrated x1.3000 vs ORION 3.0"));
        assert!(calibrated.power.c_xbar.notes.contains("calibrated"));
        // Still a valid, fully-cited profile (P3-F10: a citable profile).
        calibrated.validate(Strictness::Strict).unwrap();
    }

    #[test]
    fn calibrated_profile_round_trips_through_toml() {
        let base = db();
        let m = NativePowerModel::default();
        let node = RouterId(0);
        let calibrated = apply_dynamic_scale(&base, 2.5, "silicon");
        let text = calibrated.to_toml().unwrap();
        let reloaded = ParamDb::from_toml(&text).unwrap();

        let c = counts(3);
        let a = m.energy_from_counts(&c, node, &calibrated).total();
        let b = m.energy_from_counts(&c, node, &reloaded).total();
        assert!((a - b).abs() <= a.abs() * 1e-9);
    }

    #[test]
    fn empty_points_do_not_fit() {
        let m = NativePowerModel::default();
        let p = db();
        assert!(m.fit_dynamic_scale(&[], &p).is_none());
    }

    #[test]
    fn all_zero_activity_does_not_fit() {
        let m = NativePowerModel::default();
        let p = db();
        let points = vec![DynPoint::new(ActivityCounts::default(), 1.0e-9)];
        // Zero model prediction => nothing to scale against.
        assert!(m.fit_dynamic_scale(&points, &p).is_none());
    }
}
