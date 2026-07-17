//! `tessera-params`: the cited parameter database (P3-F6/F7/F8).
//!
//! Power, area, and thermal coefficients are **not** hard-coded in the models.
//! They live in versioned, human-editable TOML profiles where **every factor
//! carries a citation to its source** (`{value, unit, node_nm, source, notes}`),
//! so a study can plug in values from any reference, adjust them, reproduce
//! results, and cite exactly what it used (INITIAL_PRD §9A).
//!
//! The models in `tessera-power` / `tessera-thermal` read a [`ParamDb`] through
//! typed accessors; the chosen DB is [stamped](ParamDb::provenance) into the run
//! record for reproducibility (P3-F8).
//!
//! # Provenance discipline
//! Each [`Param`] requires a [`Citation`]; a param missing one fails to parse.
//! [`ParamDb::validate`] additionally enforces a [`Strictness`] policy: in
//! [`Strictness::Strict`] every citation must also carry a resolvable
//! `doi_or_url` (P3-F6, rejecting uncited/unsourced overrides).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Where a parameter value came from. The whole point of the DB: a reader can
/// see, for every number, exactly which reference it traces to.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Citation {
    /// Human-readable source label, e.g. `"ASU PTM 22nm LVT"`.
    pub label: String,
    /// DOI or URL resolving to the source. Optional in
    /// [`Strictness::Lenient`], required in [`Strictness::Strict`].
    #[serde(default)]
    pub doi_or_url: Option<String>,
}

/// A single cited coefficient (P3-F6). Generic over the value type so integer
/// counts and floating coefficients share one shape; the DB uses `Param<f64>`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Param<T> {
    pub value: T,
    /// Physical unit as written, e.g. `"F"`, `"V"`, `"J/(bit*mm)"`. Free text:
    /// the DB does not do dimensional analysis, it documents intent.
    pub unit: String,
    /// Technology node the value is for, in nm. `0` means "inherit the
    /// profile's [`ParamDb::node_nm`]" and is filled in by [`ParamDb::load`].
    #[serde(default)]
    pub node_nm: u16,
    pub source: Citation,
    #[serde(default)]
    pub notes: String,
}

impl<T> Param<T> {
    /// Construct a param programmatically (used by tests and defaults).
    pub fn new(value: T, unit: &str, node_nm: u16, label: &str, doi_or_url: Option<&str>) -> Self {
        Self {
            value,
            unit: unit.to_string(),
            node_nm,
            source: Citation {
                label: label.to_string(),
                doi_or_url: doi_or_url.map(str::to_string),
            },
            notes: String::new(),
        }
    }
}

/// Supply voltage and clock frequency for the profile's operating point.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Electrical {
    /// Supply voltage `V`; dynamic energy scales as `V²`, leakage as `V`.
    pub voltage: Param<f64>,
    /// Clock frequency (Hz); used to convert per-cycle energy to power.
    pub frequency: Param<f64>,
}

/// Per-event switched capacitances for the native dynamic-energy model
/// (`E = α·C·V²`). Capacitances are aggregated per event (per whole flit for
/// datapath events, per event for control), so the [`tessera_power`] traits
/// need no flit-width argument; the assumed flit width is recorded in
/// [`ParamDb::flit_width_bits`] and each param's `notes`.
///
/// [`tessera_power`]: https://docs.rs/tessera-power
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PowerParams {
    /// Switching activity factor `α` (dimensionless, 0..=1).
    pub alpha: Param<f64>,
    /// Buffer write (a flit entering an input FIFO).
    pub c_buf_write: Param<f64>,
    /// Buffer read (a flit leaving an input FIFO).
    pub c_buf_read: Param<f64>,
    /// Crossbar traversal.
    pub c_xbar: Param<f64>,
    /// VC-allocation arbitration event.
    pub c_va_arb: Param<f64>,
    /// Switch-allocation arbitration event.
    pub c_sa_arb: Param<f64>,
    /// Clock tree, charged once per router per cycle.
    pub c_clock: Param<f64>,
}

/// Distance-dependent link-energy parameters (P3-F3): wire capacitance per mm
/// plus repeater energy, as in ORION.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LinkParams {
    /// Wire capacitance per mm (F/mm).
    pub wire_cap_per_mm: Param<f64>,
    /// Repeater energy per bit per mm (J/(bit·mm)).
    pub repeater_e_per_bit_per_mm: Param<f64>,
    /// Physical link length for one hop (mm); default hop distance when the
    /// activity does not carry its own.
    pub tile_pitch_mm: Param<f64>,
}

/// Leakage-model parameters (P3-F2) for both selectable laws.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LeakageParams {
    /// Reference leakage current per router at `t_ref_c` (A).
    pub i_leak0: Param<f64>,
    /// Reference temperature the coefficients are pinned to (°C).
    pub t_ref_c: Param<f64>,
    /// Threshold voltage `Vth` (V): drives the subthreshold temperature form.
    pub vth: Param<f64>,
    /// Subthreshold ideality factor `η` (dimensionless).
    pub eta: Param<f64>,
    /// TEI inversion voltage (V): below it, the TEI-aware law inverts the
    /// leakage-vs-temperature slope (D5).
    pub tei_v_inv: Param<f64>,
    /// TEI slope magnitude (1/K).
    pub tei_coeff: Param<f64>,
}

/// Native area-model coefficients (P3-F5).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AreaParams {
    /// Area of one buffer bit-cell (mm²/bit).
    pub a_buf_bit: Param<f64>,
    /// Crossbar area coefficient (mm² per port²·bit).
    pub a_xbar_coeff: Param<f64>,
    /// Fixed allocator/control-logic area per router (mm²).
    pub a_arb: Param<f64>,
}

/// Material + geometry parameters for the native RC thermal network (P3-F4).
/// Carried by every profile so a profile is self-contained; consumed by
/// `tessera-thermal` (Phase 3, step 3).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ThermalParams {
    /// Silicon thermal conductivity (W/(m·K)).
    pub si_k: Param<f64>,
    /// Silicon specific heat (J/(kg·K)).
    pub si_cp: Param<f64>,
    /// Silicon density (kg/m³).
    pub si_density: Param<f64>,
    /// Active-die thickness (m).
    pub die_thickness_m: Param<f64>,
    /// Per-tile die area (m²).
    pub tile_area_m2: Param<f64>,
    /// Lumped tile-to-ambient thermal resistance (K/W).
    pub r_conv_k_per_w: Param<f64>,
    /// Ambient temperature (°C).
    pub t_amb_c: Param<f64>,
}

/// The full cited parameter database for one run. Loaded from a `params/*.toml`
/// profile; user-editable; stamped into the run record via [`Self::provenance`].
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParamDb {
    /// Profile name, e.g. `"PTM 22nm LVT"`.
    pub profile: String,
    /// Technology node (nm); inherited by any [`Param`] whose `node_nm` is 0.
    pub node_nm: u16,
    /// Flit width the aggregated per-flit capacitances assume (bits).
    pub flit_width_bits: u32,
    pub electrical: Electrical,
    pub power: PowerParams,
    pub link: LinkParams,
    pub leakage: LeakageParams,
    pub area: AreaParams,
    pub thermal: ThermalParams,
}

/// How strictly [`ParamDb::validate`] treats provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strictness {
    /// A non-empty citation label is enough.
    Lenient,
    /// Every citation must also carry a resolvable `doi_or_url` (P3-F6:
    /// reject uncited/unsourced overrides).
    Strict,
}

/// Errors from loading or validating a [`ParamDb`].
#[derive(Debug, thiserror::Error)]
pub enum ParamError {
    #[error("reading parameter profile {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing parameter profile: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("serializing parameter profile: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("parameter '{param}' has an empty citation label")]
    MissingLabel { param: String },
    #[error("parameter '{param}' (source '{label}') lacks a doi_or_url required in strict mode")]
    MissingUrl { param: String, label: String },
    #[error("parameter '{param}' is non-physical: {reason}")]
    NonPhysical { param: String, reason: String },
}

impl ParamDb {
    /// Parse a profile from TOML text, inheriting the profile `node_nm` into any
    /// param that left it at 0.
    pub fn from_toml(text: &str) -> Result<Self, ParamError> {
        let mut db: ParamDb = toml::from_str(text)?;
        db.inherit_node();
        Ok(db)
    }

    /// Load a profile from a `params/*.toml` file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ParamError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ParamError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_toml(&text)
    }

    /// Serialize back to TOML (for editing round-trips and stamping).
    pub fn to_toml(&self) -> Result<String, ParamError> {
        Ok(toml::to_string_pretty(self)?)
    }

    fn inherit_node(&mut self) {
        let node = self.node_nm;
        for p in self.each_param_mut() {
            if p.node_nm == 0 {
                p.node_nm = node;
            }
        }
    }

    /// Validate provenance (P3-F6) and physical sanity of the values.
    pub fn validate(&self, strict: Strictness) -> Result<(), ParamError> {
        for (name, p) in self.each_named_param() {
            if p.source.label.trim().is_empty() {
                return Err(ParamError::MissingLabel {
                    param: name.to_string(),
                });
            }
            if strict == Strictness::Strict && p.source.doi_or_url.is_none() {
                return Err(ParamError::MissingUrl {
                    param: name.to_string(),
                    label: p.source.label.clone(),
                });
            }
            if !p.value.is_finite() {
                return Err(ParamError::NonPhysical {
                    param: name.to_string(),
                    reason: "value is not finite".to_string(),
                });
            }
        }
        // A few cross-cutting physical invariants the models rely on.
        let a = self.power.alpha.value;
        if !(0.0..=1.0).contains(&a) {
            return Err(ParamError::NonPhysical {
                param: "power.alpha".to_string(),
                reason: format!("activity factor {a} outside 0..=1"),
            });
        }
        if self.electrical.voltage.value <= 0.0 {
            return Err(ParamError::NonPhysical {
                param: "electrical.voltage".to_string(),
                reason: "supply voltage must be positive".to_string(),
            });
        }
        if self.leakage.eta.value <= 0.0 {
            return Err(ParamError::NonPhysical {
                param: "leakage.eta".to_string(),
                reason: "ideality factor must be positive".to_string(),
            });
        }
        Ok(())
    }

    /// A human-readable provenance report stamped into the run record (P3-F8):
    /// every parameter with its value, unit, node, and source.
    pub fn provenance(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "# Parameter provenance: profile '{}' ({} nm)",
            self.profile, self.node_nm
        );
        for (name, p) in self.each_named_param() {
            let src = match &p.source.doi_or_url {
                Some(u) => format!("{} <{}>", p.source.label, u),
                None => p.source.label.clone(),
            };
            let _ = writeln!(
                s,
                "{name} = {} {} [{} nm]: {src}",
                p.value, p.unit, p.node_nm
            );
        }
        s
    }

    /// Every `Param<f64>` in the DB with its dotted name, in a fixed order
    /// (determinism: the provenance stamp and validation are reproducible).
    fn each_named_param(&self) -> Vec<(&'static str, &Param<f64>)> {
        vec![
            ("electrical.voltage", &self.electrical.voltage),
            ("electrical.frequency", &self.electrical.frequency),
            ("power.alpha", &self.power.alpha),
            ("power.c_buf_write", &self.power.c_buf_write),
            ("power.c_buf_read", &self.power.c_buf_read),
            ("power.c_xbar", &self.power.c_xbar),
            ("power.c_va_arb", &self.power.c_va_arb),
            ("power.c_sa_arb", &self.power.c_sa_arb),
            ("power.c_clock", &self.power.c_clock),
            ("link.wire_cap_per_mm", &self.link.wire_cap_per_mm),
            (
                "link.repeater_e_per_bit_per_mm",
                &self.link.repeater_e_per_bit_per_mm,
            ),
            ("link.tile_pitch_mm", &self.link.tile_pitch_mm),
            ("leakage.i_leak0", &self.leakage.i_leak0),
            ("leakage.t_ref_c", &self.leakage.t_ref_c),
            ("leakage.vth", &self.leakage.vth),
            ("leakage.eta", &self.leakage.eta),
            ("leakage.tei_v_inv", &self.leakage.tei_v_inv),
            ("leakage.tei_coeff", &self.leakage.tei_coeff),
            ("area.a_buf_bit", &self.area.a_buf_bit),
            ("area.a_xbar_coeff", &self.area.a_xbar_coeff),
            ("area.a_arb", &self.area.a_arb),
            ("thermal.si_k", &self.thermal.si_k),
            ("thermal.si_cp", &self.thermal.si_cp),
            ("thermal.si_density", &self.thermal.si_density),
            ("thermal.die_thickness_m", &self.thermal.die_thickness_m),
            ("thermal.tile_area_m2", &self.thermal.tile_area_m2),
            ("thermal.r_conv_k_per_w", &self.thermal.r_conv_k_per_w),
            ("thermal.t_amb_c", &self.thermal.t_amb_c),
        ]
    }

    fn each_param_mut(&mut self) -> Vec<&mut Param<f64>> {
        vec![
            &mut self.electrical.voltage,
            &mut self.electrical.frequency,
            &mut self.power.alpha,
            &mut self.power.c_buf_write,
            &mut self.power.c_buf_read,
            &mut self.power.c_xbar,
            &mut self.power.c_va_arb,
            &mut self.power.c_sa_arb,
            &mut self.power.c_clock,
            &mut self.link.wire_cap_per_mm,
            &mut self.link.repeater_e_per_bit_per_mm,
            &mut self.link.tile_pitch_mm,
            &mut self.leakage.i_leak0,
            &mut self.leakage.t_ref_c,
            &mut self.leakage.vth,
            &mut self.leakage.eta,
            &mut self.leakage.tei_v_inv,
            &mut self.leakage.tei_coeff,
            &mut self.area.a_buf_bit,
            &mut self.area.a_xbar_coeff,
            &mut self.area.a_arb,
            &mut self.thermal.si_k,
            &mut self.thermal.si_cp,
            &mut self.thermal.si_density,
            &mut self.thermal.die_thickness_m,
            &mut self.thermal.tile_area_m2,
            &mut self.thermal.r_conv_k_per_w,
            &mut self.thermal.t_amb_c,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PTM: &str = include_str!("../../../params/ptm_22nm.toml");
    const ITRS: &str = include_str!("../../../params/itrs_45nm.toml");

    #[test]
    fn shipped_profiles_load_and_validate() {
        for text in [PTM, ITRS] {
            let db = ParamDb::from_toml(text).expect("profile parses");
            db.validate(Strictness::Strict)
                .expect("shipped profiles are fully cited with URLs");
        }
    }

    #[test]
    fn node_is_inherited_when_param_omits_it() {
        let db = ParamDb::from_toml(PTM).unwrap();
        assert_eq!(db.node_nm, 22);
        // No param left its node at the sentinel 0 after load.
        assert!(db.each_named_param().iter().all(|(_, p)| p.node_nm != 0));
        assert_eq!(db.electrical.voltage.node_nm, 22);
    }

    #[test]
    fn toml_round_trips() {
        let db = ParamDb::from_toml(PTM).unwrap();
        let text = db.to_toml().unwrap();
        let db2 = ParamDb::from_toml(&text).unwrap();
        assert_eq!(db.profile, db2.profile);
        assert_eq!(db.electrical.voltage.value, db2.electrical.voltage.value);
        assert_eq!(
            db.leakage.tei_v_inv.source.label,
            db2.leakage.tei_v_inv.source.label
        );
    }

    #[test]
    fn strict_mode_rejects_missing_url() {
        let mut db = ParamDb::from_toml(PTM).unwrap();
        db.electrical.voltage.source.doi_or_url = None;
        // Lenient still accepts a labelled-but-unlinked citation...
        db.validate(Strictness::Lenient).unwrap();
        // ...strict rejects it (P3-F6).
        let err = db.validate(Strictness::Strict).unwrap_err();
        assert!(matches!(err, ParamError::MissingUrl { .. }));
    }

    #[test]
    fn empty_label_is_rejected_even_leniently() {
        let mut db = ParamDb::from_toml(PTM).unwrap();
        db.power.alpha.source.label = "   ".to_string();
        assert!(matches!(
            db.validate(Strictness::Lenient).unwrap_err(),
            ParamError::MissingLabel { .. }
        ));
    }

    #[test]
    fn non_physical_alpha_is_rejected() {
        let mut db = ParamDb::from_toml(PTM).unwrap();
        db.power.alpha.value = 1.5;
        assert!(matches!(
            db.validate(Strictness::Lenient).unwrap_err(),
            ParamError::NonPhysical { .. }
        ));
    }

    #[test]
    fn provenance_stamp_names_every_param_and_its_source() {
        let db = ParamDb::from_toml(PTM).unwrap();
        let stamp = db.provenance();
        assert!(stamp.contains("PTM 22nm LVT"));
        // Every named param has exactly one `name = ...` data line.
        let params = db.each_named_param();
        for (name, _) in &params {
            let lines = stamp
                .lines()
                .filter(|l| l.starts_with(&format!("{name} = ")))
                .count();
            assert_eq!(lines, 1, "param {name} should be stamped on one line");
        }
        // One header line + one line per param, nothing extra.
        assert_eq!(stamp.lines().count(), params.len() + 1);
    }
}
