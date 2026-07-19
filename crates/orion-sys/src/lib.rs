//! `orion-sys`: FFI seam for ORION 3.0 router power & area (P3-F9, legacy).
//!
//! ORION 3.0 (RESEARCH_NOTES D1) is regression-fit to physical-implementation
//! data (avg error ≤ 9.3%) and is PAT-Noxim's power model; here it is the
//! legacy/comparison reference behind the same [`PowerModel`]/[`AreaModel`]
//! traits as the native model.
//!
//! The external ORION library is bound behind the opt-in `link` feature. Without
//! it (the default) the model reports **degraded** via [`OrionPower::try_new`]
//! and callers fall back to native. See [`dsent-sys`](../dsent_sys) for the seam
//! rationale; real bindings are pending the library.

use tessera_core::RouterId;
use tessera_params::ParamDb;
use tessera_power::{Activity, AreaModel, Joules, Mm2, PowerModel, RouterCfg, Watts};

/// True when built with the `link` feature (external library requested).
pub const LINKED: bool = cfg!(feature = "link");
/// Display name of the reference model.
pub const MODEL: &str = "ORION 3.0";

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

/// ORION 3.0-backed power & area model (P3-F9).
#[derive(Debug)]
pub struct OrionPower {
    _private: (),
}

impl OrionPower {
    /// Construct an ORION-backed model, or report why it is unavailable.
    pub fn try_new() -> Result<Self, Unavailable> {
        Err(Unavailable {
            model: MODEL,
            reason: if LINKED {
                "linked but ORION bindings are not yet implemented (P3-F9)"
            } else {
                "built without the `link` feature; ORION library not bound"
            },
        })
    }
}

impl PowerModel for OrionPower {
    fn dynamic_energy(&self, _activity: Activity, _node: RouterId, _p: &ParamDb) -> Joules {
        todo!("bind ORION dynamic-energy model")
    }

    fn leakage_power(&self, _node: RouterId, _temp_c: f32, _p: &ParamDb) -> Watts {
        todo!("bind ORION leakage model")
    }
}

impl AreaModel for OrionPower {
    fn router_area(&self, _cfg: &RouterCfg, _p: &ParamDb) -> Mm2 {
        todo!("bind ORION area model")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_degraded_without_the_library() {
        let err = OrionPower::try_new().unwrap_err();
        assert_eq!(err.model, "ORION 3.0");
        assert!(!err.reason.is_empty());
    }

    #[test]
    fn implements_the_power_and_area_traits() {
        fn assert_impl<T: PowerModel + AreaModel>() {}
        assert_impl::<OrionPower>();
    }
}
