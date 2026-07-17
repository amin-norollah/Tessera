//! `dsent-sys` — FFI seam for DSENT electrical power & area (P3-F9, primary).
//!
//! DSENT (RESEARCH_NOTES D2) is the primary modern reference power/area model
//! (electrical, ~20% of SPICE). This crate is the **trait-conforming seam**:
//! [`DsentPower`] implements the same [`PowerModel`]/[`AreaModel`] traits as the
//! native model, so a run can name it interchangeably.
//!
//! The external DSENT C++ library is bound behind the opt-in `link` feature.
//! Without it — the default, so the workspace builds with no library present —
//! the model reports **degraded** via [`DsentPower::try_new`], and callers fall
//! back to the native `tessera-power` model. Real bindings are pending the
//! library (the trait bodies are `todo!` until then); the seam is compile-checked
//! today so wiring the C++ side later touches no caller.

use tessera_core::RouterId;
use tessera_params::ParamDb;
use tessera_power::{Activity, AreaModel, Joules, Mm2, PowerModel, RouterCfg, Watts};

/// True when built with the `link` feature (external library requested).
pub const LINKED: bool = cfg!(feature = "link");
/// Display name of the reference model.
pub const MODEL: &str = "DSENT";

/// The reference model is unavailable; the caller should fall back to native.
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

/// DSENT-backed power & area model (P3-F9).
#[derive(Debug)]
pub struct DsentPower {
    _private: (),
}

impl DsentPower {
    /// Construct a DSENT-backed model, or report why it is unavailable.
    ///
    /// Returns [`Unavailable`] both when built without `link` (no library) and,
    /// for now, when built with `link` (bindings pending) — so callers always
    /// have a defined fallback path.
    pub fn try_new() -> Result<Self, Unavailable> {
        Err(Unavailable {
            model: MODEL,
            reason: if LINKED {
                "linked but DSENT bindings are not yet implemented (P3-F9)"
            } else {
                "built without the `link` feature; DSENT library not bound"
            },
        })
    }
}

impl PowerModel for DsentPower {
    fn dynamic_energy(&self, _activity: Activity, _node: RouterId, _p: &ParamDb) -> Joules {
        todo!("bind DSENT dynamic-energy model")
    }

    fn leakage_power(&self, _node: RouterId, _temp_c: f32, _p: &ParamDb) -> Watts {
        todo!("bind DSENT leakage model")
    }
}

impl AreaModel for DsentPower {
    fn router_area(&self, _cfg: &RouterCfg, _p: &ParamDb) -> Mm2 {
        todo!("bind DSENT area model")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_degraded_without_the_library() {
        let err = DsentPower::try_new().unwrap_err();
        assert_eq!(err.model, "DSENT");
        assert!(!err.reason.is_empty());
    }

    #[test]
    fn implements_the_power_and_area_traits() {
        // Compile-time proof the seam conforms to both model traits.
        fn assert_impl<T: PowerModel + AreaModel>() {}
        assert_impl::<DsentPower>();
    }
}
