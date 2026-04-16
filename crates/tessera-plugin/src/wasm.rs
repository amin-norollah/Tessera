//! Sandboxed WebAssembly routing plugins (opt-in `wasm` feature).
//!
//! A `.wasm` (or WAT) module exporting `route(dx: i32, dy: i32) -> i32` is
//! loaded into a wasmtime engine and driven through the [`RoutingAlgorithm`]
//! contract like any native plugin. The ABI is flat integers only:
//! `dx = dst.x - here.x`, `dy = dst.y - here.y`, and the returned code selects a
//! direction (0=+X, 1=-X, 2=+Y, 3=-Y, else Local). No pointers or floats cross
//! the boundary, so results are deterministic and portable.
//!
//! **Sandbox:** the module is instantiated with an empty import set, so it has
//! no host functions, no WASI, and therefore no filesystem or network access.
//! Execution is metered by fuel, so a runaway/looping plugin traps instead of
//! hanging the host.

use std::sync::Mutex;

use tessera_core::{Candidates, PortLabel, RouteCtx, RoutingAlgorithm};
use wasmtime::{Config, Engine, Instance, Module, Store, TypedFunc};

use crate::Registry;

/// Fuel budget per `route` call. A correct routing function uses a handful of
/// instructions; a looping plugin exhausts this and traps.
const FUEL_PER_CALL: u64 = 100_000;

const CODE_X_PLUS: i32 = 0;
const CODE_X_MINUS: i32 = 1;
const CODE_Y_PLUS: i32 = 2;
const CODE_Y_MINUS: i32 = 3;

/// An error loading or calling a WASM routing plugin.
#[derive(Clone, Debug)]
pub struct WasmError(pub String);

impl std::fmt::Display for WasmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "wasm plugin error: {}", self.0)
    }
}

impl std::error::Error for WasmError {}

fn werr(e: impl std::fmt::Display) -> WasmError {
    WasmError(e.to_string())
}

struct WasmState {
    store: Store<()>,
    route: TypedFunc<(i32, i32), i32>,
}

/// A routing algorithm backed by a sandboxed WASM module.
pub struct WasmRouting {
    // Mutex makes the (Send-but-not-Sync) store usable behind the Send + Sync
    // `RoutingAlgorithm` contract. Routing plugins are for teaching / shareable
    // artifacts, not the hot production path, so the per-call lock is fine.
    state: Mutex<WasmState>,
}

impl WasmRouting {
    /// Load from WebAssembly text.
    pub fn from_wat(wat: &str) -> Result<Self, WasmError> {
        Self::build(wat.as_bytes())
    }

    /// Load from a compiled `.wasm` binary.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WasmError> {
        Self::build(bytes)
    }

    fn build(module_bytes: &[u8]) -> Result<Self, WasmError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(werr)?;
        let module = Module::new(&engine, module_bytes).map_err(werr)?;

        let mut store = Store::new(&engine, ());
        store.set_fuel(FUEL_PER_CALL).map_err(werr)?;
        // Empty import set: no host functions, no WASI -> no ambient authority.
        let instance = Instance::new(&mut store, &module, &[]).map_err(werr)?;
        let route = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "route")
            .map_err(werr)?;

        Ok(Self {
            state: Mutex::new(WasmState { store, route }),
        })
    }

    /// Call the plugin's `route` with the raw ABI. Returns an error if the
    /// plugin traps (e.g. exhausts its fuel).
    pub fn route_raw(&self, dx: i32, dy: i32) -> Result<i32, WasmError> {
        let mut guard = self.state.lock().expect("wasm plugin mutex poisoned");
        let state = &mut *guard;
        state.store.set_fuel(FUEL_PER_CALL).map_err(werr)?;
        state.route.call(&mut state.store, (dx, dy)).map_err(werr)
    }
}

fn code_to_label(code: i32) -> PortLabel {
    match code {
        CODE_X_PLUS => PortLabel::XPlus,
        CODE_X_MINUS => PortLabel::XMinus,
        CODE_Y_PLUS => PortLabel::YPlus,
        CODE_Y_MINUS => PortLabel::YMinus,
        _ => PortLabel::Local,
    }
}

impl RoutingAlgorithm for WasmRouting {
    fn route(&self, ctx: &RouteCtx) -> Candidates {
        let here = ctx.topo.coord(ctx.here);
        let dst = ctx.topo.coord(ctx.dst);
        let dx = dst.x as i32 - here.x as i32;
        let dy = dst.y as i32 - here.y as i32;
        // A trapping plugin degrades to local delivery rather than panicking;
        // such a plugin fails the conformance harness.
        let code = self.route_raw(dx, dy).unwrap_or(i32::MAX);
        let label = code_to_label(code);
        let port = ctx
            .topo
            .port_for_label(ctx.here, label)
            .or_else(|| ctx.topo.port_for_label(ctx.here, PortLabel::Local))
            .expect("the local port always exists");
        Candidates::single(port)
    }

    fn is_deterministic(&self) -> bool {
        true
    }
}

/// Validate a WAT routing plugin, then register it under `key` so it can be
/// selected by string like any other routing component.
pub fn register_wasm_routing(reg: &mut Registry, key: &str, wat: &str) -> Result<(), WasmError> {
    WasmRouting::from_wat(wat)?; // fail fast on a bad module
    let owned = wat.to_string();
    reg.register_routing(
        key,
        Box::new(move |_| -> Box<dyn RoutingAlgorithm> {
            Box::new(WasmRouting::from_wat(&owned).expect("validated at registration"))
        }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{builtin_registry, conformance, PluginCtx};
    use tessera_core::{RouterId, Topology};
    use tessera_model::Mesh2D;
    use tessera_routing::DimensionOrderXy;

    /// XY routing expressed directly in WebAssembly text.
    const XY_WAT: &str = r#"
        (module
          (func (export "route") (param $dx i32) (param $dy i32) (result i32)
            (if (result i32) (i32.gt_s (local.get $dx) (i32.const 0))
              (then (i32.const 0))
              (else (if (result i32) (i32.lt_s (local.get $dx) (i32.const 0))
                (then (i32.const 1))
                (else (if (result i32) (i32.gt_s (local.get $dy) (i32.const 0))
                  (then (i32.const 2))
                  (else (if (result i32) (i32.lt_s (local.get $dy) (i32.const 0))
                    (then (i32.const 3))
                    (else (i32.const 4)))))))))))
    "#;

    #[test]
    fn wasm_router_passes_conformance() {
        let router = WasmRouting::from_wat(XY_WAT).expect("valid module");
        conformance::check_routing(&router, &Mesh2D::new(5, 4)).expect("wasm router conforms");
    }

    #[test]
    fn wasm_router_matches_native_xy() {
        // Bit-for-bit equivalence between the sandboxed plugin and native XY
        // (P2-F6 / G2.3): identical port on every source-destination pair.
        let topo = Mesh2D::new(6, 5);
        let wasm = WasmRouting::from_wat(XY_WAT).unwrap();
        let native = DimensionOrderXy;
        let local = topo.local_port();
        for h in 0..topo.num_routers() {
            for d in 0..topo.num_routers() {
                let ctx = RouteCtx {
                    here: RouterId(h),
                    dst: RouterId(d),
                    in_port: local,
                    topo: &topo,
                };
                assert_eq!(
                    wasm.route(&ctx).get(0).0,
                    native.route(&ctx).get(0).0,
                    "wasm != native at here={h} dst={d}"
                );
            }
        }
    }

    #[test]
    fn wasm_router_registers_and_builds_by_key() {
        let mut reg = builtin_registry();
        register_wasm_routing(&mut reg, "xy-wasm", XY_WAT).expect("registers");
        assert!(reg.has_routing("xy-wasm"));
        let ctx = PluginCtx {
            num_nodes: 16,
            num_vcs: 1,
            packet_size: 4,
            injection_rate: 0.1,
        };
        assert!(reg.build_routing("xy-wasm", &ctx).is_some());
    }

    #[test]
    fn sandbox_rejects_modules_that_import_host_functions() {
        // A module requesting an import gets none (empty import set) and fails
        // to instantiate — it cannot reach the host.
        let evil = r#"
            (module
              (import "env" "exfiltrate" (func))
              (func (export "route") (param i32 i32) (result i32) (i32.const 0)))
        "#;
        assert!(WasmRouting::from_wat(evil).is_err());
    }

    #[test]
    fn sandbox_fuel_traps_a_runaway_plugin() {
        let looping = r#"
            (module
              (func (export "route") (param i32 i32) (result i32)
                (loop $l (br $l))
                (i32.const 0)))
        "#;
        let router = WasmRouting::from_wat(looping).expect("module loads");
        // The infinite loop exhausts the fuel budget and traps.
        assert!(router.route_raw(1, 1).is_err());
    }
}
