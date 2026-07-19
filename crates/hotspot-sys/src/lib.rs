//! `hotspot-sys`: FFI seam for the HotSpot 6.0 RC thermal solver (P3-F9).
//!
//! HotSpot 6.0 (RESEARCH_NOTES D4) is the reference compact RC thermal solver
//! Tessera's native `tessera-thermal` model mirrors. This crate exposes it behind
//! the same [`ThermalModel`] trait for calibration and comparison.
//!
//! The external HotSpot library is bound behind the opt-in `link` feature.
//! Without it (the default) the solver reports **degraded** via
//! [`HotSpotThermal::try_new`] and callers fall back to the native RC network.
//! Real bindings are pending the library; the seam is compile-checked today.

use tessera_thermal::{Celsius, ThermalModel, Watts};

/// True when built with the `link` feature (external library requested).
pub const LINKED: bool = cfg!(feature = "link");
/// Display name of the reference model.
pub const MODEL: &str = "HotSpot 6.0";

/// The reference solver is unavailable; the caller should fall back to native.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub model: &'static str,
    pub reason: &'static str,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} model unavailable: {}", self.model, self.reason)
    }
}

impl std::error::Error for Unavailable {}

/// HotSpot 6.0-backed thermal solver (P3-F9).
#[derive(Debug)]
pub struct HotSpotThermal {
    _private: (),
}

impl HotSpotThermal {
    /// Construct a HotSpot-backed solver, or report why it is unavailable.
    pub fn try_new() -> Result<Self, Unavailable> {
        Err(Unavailable {
            model: MODEL,
            reason: if LINKED {
                "linked but HotSpot bindings are not yet implemented (P3-F9)"
            } else {
                "built without the `link` feature; HotSpot library not bound"
            },
        })
    }
}

impl ThermalModel for HotSpotThermal {
    fn step(&mut self, _power_w: &[Watts], _dt_s: f32) -> &[Celsius] {
        todo!("bind HotSpot transient solve")
    }

    fn temperatures(&self) -> &[Celsius] {
        todo!("bind HotSpot temperature readout")
    }

    fn reset(&mut self) {
        todo!("bind HotSpot reset")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_degraded_without_the_library() {
        let err = HotSpotThermal::try_new().unwrap_err();
        assert_eq!(err.model, "HotSpot 6.0");
        assert!(!err.reason.is_empty());
    }

    #[test]
    fn implements_the_thermal_trait() {
        fn assert_impl<T: ThermalModel>() {}
        assert_impl::<HotSpotThermal>();
    }
}
