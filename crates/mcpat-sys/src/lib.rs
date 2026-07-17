//! `mcpat-sys` — FFI seam for McPAT power/area/timing (P3-F9/F11).
//!
//! McPAT (RESEARCH_NOTES D3) provides integrated power/area/timing and is the
//! reference behind processing-element power in PAT-Noxim. It is exposed here
//! behind the same [`PowerModel`]/[`AreaModel`] traits as the native model.
//!
//! **P3-F11 (MUST):** PAT-Noxim's McPAT wiring is under-documented and must be
//! **verified against source** before McPAT output is trusted. Until that
//! verification is done and real bindings land, this crate reports **degraded**
//! and is explicitly marked as such (rather than silently returning numbers).
//! [`MCPAT_WIRING_VERIFIED`] records that status for callers and audits.

use tessera_core::RouterId;
use tessera_params::ParamDb;
use tessera_power::{Activity, AreaModel, Joules, Mm2, PowerModel, RouterCfg, Watts};

/// True when built with the `link` feature (external library requested).
pub const LINKED: bool = cfg!(feature = "link");
/// Display name of the reference model.
pub const MODEL: &str = "McPAT";
/// Whether the McPAT integration has been verified against source (P3-F11).
/// `false` until the wiring is audited and real bindings land — callers must
/// treat McPAT output as untrusted while this is `false`.
pub const MCPAT_WIRING_VERIFIED: bool = false;

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

/// McPAT-backed power & area model (P3-F9/F11).
#[derive(Debug)]
pub struct McpatPower {
    _private: (),
}

impl McpatPower {
    /// Construct a McPAT-backed model, or report why it is unavailable.
    ///
    /// Reports **degraded** while [`MCPAT_WIRING_VERIFIED`] is `false` even if
    /// `link` is set: per P3-F11 McPAT must not be trusted until its wiring is
    /// verified against source.
    pub fn try_new() -> Result<Self, Unavailable> {
        Err(Unavailable {
            model: MODEL,
            reason: if !LINKED {
                "built without the `link` feature; McPAT library not bound"
            } else {
                "McPAT wiring not yet verified against source (P3-F11); marked degraded"
            },
        })
    }
}

impl PowerModel for McpatPower {
    fn dynamic_energy(&self, _activity: Activity, _node: RouterId, _p: &ParamDb) -> Joules {
        todo!("bind + verify McPAT dynamic-energy model (P3-F11)")
    }

    fn leakage_power(&self, _node: RouterId, _temp_c: f32, _p: &ParamDb) -> Watts {
        todo!("bind + verify McPAT leakage model (P3-F11)")
    }
}

impl AreaModel for McpatPower {
    fn router_area(&self, _cfg: &RouterCfg, _p: &ParamDb) -> Mm2 {
        todo!("bind + verify McPAT area model (P3-F11)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_degraded_and_unverified() {
        let err = McpatPower::try_new().unwrap_err();
        assert_eq!(err.model, "McPAT");
        assert!(!err.reason.is_empty());
        // P3-F11: never claim verified until the wiring is audited. This is a
        // deliberate guard on the constant, not a runtime condition.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(!MCPAT_WIRING_VERIFIED);
        }
    }

    #[test]
    fn implements_the_power_and_area_traits() {
        fn assert_impl<T: PowerModel + AreaModel>() {}
        assert_impl::<McpatPower>();
    }
}
