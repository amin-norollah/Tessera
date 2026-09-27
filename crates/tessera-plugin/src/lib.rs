//! The Tessera plugin API (v1).
//!
//! Components (routing, selection, traffic) are named by string key and built
//! through factory closures held in a [`Registry`]. Built-ins register through
//! the same path as third-party plugins (dogfooding), so a contributor can add
//! an algorithm by registering it under a new key: without touching the engine
//! core. The [`conformance`] harness checks that any implementation honors its
//! trait contract.
//!
//! Frozen at [`PLUGIN_API_VERSION`] `1`: `RoutingAlgorithm`, `SelectionPolicy`,
//! and `TrafficGenerator` (all defined in `tessera-core`). The `Allocator` and
//! `StatisticsCollector` traits join the freeze once their engine
//! implementations land (Phase 1 allocator work / Phase 3 stats). The wasmtime
//! WASM host is a follow-up increment; the registry below is the native path it
//! will mirror.

use std::collections::HashMap;

use tessera_core::{RoutingAlgorithm, SelectionPolicy, TrafficGenerator};
use tessera_routing::{
    BufferLevel, DimensionOrderXy, DimensionOrderXyz, FirstAvailable, NegativeFirst, NorthLast,
    OddEven, Random, RoundRobinSelection, WestFirst,
};
use tessera_traffic::Uniform;

pub mod conformance;
#[cfg(feature = "wasm")]
pub mod wasm;

/// The frozen plugin-API major version. Bumping this is a breaking change;
/// additive capabilities within a major stay backward-compatible.
pub const PLUGIN_API_VERSION: u32 = 1;

/// Whether a plugin built against `wanted` is compatible with this host.
pub fn api_compatible(wanted: u32) -> bool {
    wanted == PLUGIN_API_VERSION
}

/// Parameters a factory may need to build a component instance.
#[derive(Clone, Copy, Debug)]
pub struct PluginCtx {
    pub num_nodes: u32,
    pub num_vcs: u8,
    pub packet_size: u16,
    pub injection_rate: f64,
}

type RoutingFactory = Box<dyn Fn(&PluginCtx) -> Box<dyn RoutingAlgorithm> + Send + Sync>;
type SelectionFactory = Box<dyn Fn(&PluginCtx) -> Box<dyn SelectionPolicy> + Send + Sync>;
type TrafficFactory = Box<dyn Fn(&PluginCtx) -> Box<dyn TrafficGenerator> + Send + Sync>;

/// A string-keyed registry of component factories.
#[derive(Default)]
pub struct Registry {
    routing: HashMap<String, RoutingFactory>,
    selection: HashMap<String, SelectionFactory>,
    traffic: HashMap<String, TrafficFactory>,
    installed: Vec<String>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a plugin after checking its declared API version is compatible
    /// with this host. Incompatible plugins are rejected without registering
    /// anything.
    pub fn install(&mut self, plugin: &dyn Plugin) -> Result<(), PluginError> {
        let manifest = plugin.manifest();
        if !api_compatible(manifest.api_version) {
            return Err(PluginError::IncompatibleApiVersion {
                plugin: manifest.name,
                wanted: manifest.api_version,
                host: PLUGIN_API_VERSION,
            });
        }
        plugin.register(self);
        self.installed.push(manifest.name);
        Ok(())
    }

    /// Names of the plugins installed via [`Registry::install`].
    pub fn installed(&self) -> &[String] {
        &self.installed
    }

    pub fn register_routing(&mut self, name: impl Into<String>, factory: RoutingFactory) {
        self.routing.insert(name.into(), factory);
    }

    pub fn register_selection(&mut self, name: impl Into<String>, factory: SelectionFactory) {
        self.selection.insert(name.into(), factory);
    }

    pub fn register_traffic(&mut self, name: impl Into<String>, factory: TrafficFactory) {
        self.traffic.insert(name.into(), factory);
    }

    pub fn build_routing(&self, name: &str, ctx: &PluginCtx) -> Option<Box<dyn RoutingAlgorithm>> {
        self.routing.get(name).map(|f| f(ctx))
    }

    pub fn build_selection(&self, name: &str, ctx: &PluginCtx) -> Option<Box<dyn SelectionPolicy>> {
        self.selection.get(name).map(|f| f(ctx))
    }

    pub fn build_traffic(&self, name: &str, ctx: &PluginCtx) -> Option<Box<dyn TrafficGenerator>> {
        self.traffic.get(name).map(|f| f(ctx))
    }

    pub fn has_routing(&self, name: &str) -> bool {
        self.routing.contains_key(name)
    }

    pub fn has_selection(&self, name: &str) -> bool {
        self.selection.contains_key(name)
    }

    pub fn routing_names(&self) -> Vec<&str> {
        sorted_keys(&self.routing)
    }

    pub fn selection_names(&self) -> Vec<&str> {
        sorted_keys(&self.selection)
    }

    pub fn traffic_names(&self) -> Vec<&str> {
        sorted_keys(&self.traffic)
    }
}

fn sorted_keys<V>(map: &HashMap<String, V>) -> Vec<&str> {
    let mut names: Vec<&str> = map.keys().map(String::as_str).collect();
    names.sort_unstable();
    names
}

/// What a plugin declares about itself when installed.
#[derive(Clone, Debug)]
pub struct PluginManifest {
    pub name: String,
    /// The plugin-API major version this plugin was built against.
    pub api_version: u32,
}

/// A bundle of components installed together under one manifest. Built-ins use
/// the same path as third-party plugins.
pub trait Plugin {
    fn manifest(&self) -> PluginManifest;
    fn register(&self, reg: &mut Registry);
}

/// Errors from installing a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginError {
    IncompatibleApiVersion {
        plugin: String,
        wanted: u32,
        host: u32,
    },
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::IncompatibleApiVersion {
                plugin,
                wanted,
                host,
            } => write!(
                f,
                "plugin '{plugin}' targets API v{wanted}, but this host is API v{host}"
            ),
        }
    }
}

impl std::error::Error for PluginError {}

/// The in-tree components, packaged as a plugin so built-ins install through
/// the same path third-party plugins use.
struct Builtins;

impl Plugin for Builtins {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "tessera-builtins".to_string(),
            api_version: PLUGIN_API_VERSION,
        }
    }

    fn register(&self, r: &mut Registry) {
        r.register_routing(
            "xy",
            Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(DimensionOrderXy) }),
        );
        r.register_routing(
            "xyz",
            Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(DimensionOrderXyz) }),
        );
        r.register_routing(
            "west-first",
            Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(WestFirst) }),
        );
        r.register_routing(
            "north-last",
            Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(NorthLast) }),
        );
        r.register_routing(
            "negative-first",
            Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(NegativeFirst) }),
        );
        r.register_routing(
            "odd-even",
            Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(OddEven) }),
        );
        r.register_selection(
            "first-available",
            Box::new(|_| -> Box<dyn SelectionPolicy> { Box::new(FirstAvailable) }),
        );
        r.register_selection(
            "buffer-level",
            Box::new(|_| -> Box<dyn SelectionPolicy> { Box::new(BufferLevel) }),
        );
        r.register_selection(
            "round-robin",
            Box::new(|_| -> Box<dyn SelectionPolicy> { Box::new(RoundRobinSelection::new()) }),
        );
        r.register_selection(
            "random",
            Box::new(|_| -> Box<dyn SelectionPolicy> { Box::new(Random) }),
        );
        r.register_traffic(
            "uniform",
            Box::new(|c: &PluginCtx| -> Box<dyn TrafficGenerator> {
                Box::new(Uniform::new(c.num_nodes, c.injection_rate, c.packet_size))
            }),
        );
    }
}

/// A registry pre-populated with the in-tree components. Third-party plugins
/// install additional keys on top of this via [`Registry::install`].
pub fn builtin_registry() -> Registry {
    let mut r = Registry::new();
    r.install(&Builtins)
        .expect("built-ins target the current API version");
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_core::{Candidates, PortLabel, RouteCtx, RoutingAlgorithm};
    use tessera_model::Mesh2D;

    fn ctx() -> PluginCtx {
        PluginCtx {
            num_nodes: 16,
            num_vcs: 1,
            packet_size: 4,
            injection_rate: 0.1,
        }
    }

    #[test]
    fn version_negotiation() {
        assert!(api_compatible(PLUGIN_API_VERSION));
        assert!(!api_compatible(PLUGIN_API_VERSION + 1));
    }

    #[test]
    fn builtins_are_registered_and_named() {
        let r = builtin_registry();
        assert_eq!(
            r.routing_names(),
            [
                "negative-first",
                "north-last",
                "odd-even",
                "west-first",
                "xy",
                "xyz"
            ]
        );
        assert_eq!(
            r.selection_names(),
            ["buffer-level", "first-available", "random", "round-robin"]
        );
        assert_eq!(r.traffic_names(), ["uniform"]);
        assert!(r.build_routing("xy", &ctx()).is_some());
        assert!(r.build_routing("does-not-exist", &ctx()).is_none());
    }

    #[test]
    fn builtin_routers_pass_conformance() {
        let topo = Mesh2D::new(5, 4);
        let r = builtin_registry();
        for name in r.routing_names() {
            let router = r.build_routing(name, &ctx()).unwrap();
            conformance::check_routing(router.as_ref(), &topo)
                .unwrap_or_else(|e| panic!("{name} failed conformance: {e}"));
        }
    }

    /// A third-party routing algorithm defined entirely outside the core, then
    /// registered by key and validated: the Phase 2 community gate (G2.1).
    struct YxRouting;
    impl RoutingAlgorithm for YxRouting {
        fn route(&self, c: &RouteCtx) -> Candidates {
            let here = c.topo.coord(c.here);
            let dst = c.topo.coord(c.dst);
            let label = if dst.y > here.y {
                PortLabel::YPlus
            } else if dst.y < here.y {
                PortLabel::YMinus
            } else if dst.x > here.x {
                PortLabel::XPlus
            } else if dst.x < here.x {
                PortLabel::XMinus
            } else {
                PortLabel::Local
            };
            Candidates::single(c.topo.port_for_label(c.here, label).unwrap())
        }
    }

    #[test]
    fn third_party_plugin_registers_and_conforms() {
        let mut r = builtin_registry();
        r.register_routing(
            "yx",
            Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(YxRouting) }),
        );
        assert!(r.has_routing("yx"));
        let router = r.build_routing("yx", &ctx()).expect("registered by key");
        conformance::check_routing(router.as_ref(), &Mesh2D::new(5, 4))
            .expect("custom router conforms");
    }

    /// A third-party plugin bundling one router under a declared API version.
    struct YxPlugin {
        api_version: u32,
    }
    impl Plugin for YxPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                name: "yx-router".to_string(),
                api_version: self.api_version,
            }
        }
        fn register(&self, reg: &mut Registry) {
            reg.register_routing(
                "yx",
                Box::new(|_| -> Box<dyn RoutingAlgorithm> { Box::new(YxRouting) }),
            );
        }
    }

    #[test]
    fn install_rejects_incompatible_api_version() {
        let mut r = builtin_registry();
        let err = r
            .install(&YxPlugin {
                api_version: PLUGIN_API_VERSION + 1,
            })
            .unwrap_err();
        assert!(matches!(err, PluginError::IncompatibleApiVersion { .. }));
        assert!(!r.has_routing("yx"), "rejected plugin registers nothing");
    }

    #[test]
    fn install_accepts_compatible_plugin() {
        let mut r = builtin_registry();
        r.install(&YxPlugin {
            api_version: PLUGIN_API_VERSION,
        })
        .unwrap();
        assert!(r.has_routing("yx"));
        assert!(r.installed().iter().any(|n| n == "yx-router"));
        assert!(r.installed().iter().any(|n| n == "tessera-builtins"));
    }

    #[test]
    fn builtin_selections_pass_conformance() {
        let r = builtin_registry();
        let mut cands = Candidates::new();
        cands.push(tessera_core::PortId(1));
        cands.push(tessera_core::PortId(3));
        for name in r.selection_names() {
            let mut policy = r.build_selection(name, &ctx()).unwrap();
            for _ in 0..4 {
                conformance::check_selection(policy.as_mut(), &cands)
                    .unwrap_or_else(|e| panic!("{name} failed conformance: {e}"));
            }
        }
    }

    #[test]
    fn registry_conformance_runner_checks_all_routers() {
        let r = builtin_registry();
        conformance::check_all_routing(&r, &Mesh2D::new(5, 4), &ctx())
            .expect("all built-ins conform");
    }
}
