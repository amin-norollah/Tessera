//! `tessera-config`: the runtime config schema, pre-run `validate()` pass,
//! and Noxim traffic/routing-table import (`docs/phases/PHASE_1_PRD.md`
//! P1-F13–F16).
//!
//! Sits between the network-behavior crates and the plugin/cli layer in the
//! dependency graph (`core -> model -> routing/traffic -> config -> plugin ->
//! cli`, `INITIAL_PRD.md` §20.1): it only depends on `tessera-core` for the
//! handle types (`RouterId`, `PortId`) that appear in parsed rows, so it never
//! needs to know about concrete routing/traffic implementations or the plugin
//! registry that resolves them by string key.

pub mod noxim;
pub mod schema;
pub mod validate;

pub use noxim::{
    parse_routing_table, parse_traffic_table, NoximImportError, NoximRoutingRow, NoximTrafficRow,
};
pub use schema::{
    AllocatorCfg, NodeMappingCfg, PacketSizeCfg, RouterCfg, SimConfigFile, TopologyCfg, TrafficCfg,
    TrafficKind, VcClassCfg, CURRENT_VERSION,
};
pub use validate::{validate, ConfigError};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let cfg = SimConfigFile::default();
        let text = toml::to_string(&cfg).expect("serializes");
        let reparsed: SimConfigFile = toml::from_str(&text).expect("reparses");
        assert_eq!(cfg, reparsed);
    }

    #[test]
    fn round_trips_a_non_default_config() {
        let cfg = SimConfigFile {
            topology: TopologyCfg::Torus2D {
                width: 6,
                height: 6,
            },
            router: RouterCfg {
                num_vcs: 4,
                vc_classes: Some(VcClassCfg {
                    escape: 1,
                    adaptive: 3,
                }),
                dateline: true,
                routing: "adaptive".to_string(),
                allocator_va: AllocatorCfg::ISlip,
                ..RouterCfg::default()
            },
            traffic: TrafficCfg {
                kind: TrafficKind::TableBased {
                    path: "traffic.txt".to_string(),
                },
                packet_size: PacketSizeCfg::Bimodal {
                    control: 1,
                    data: 8,
                    data_ratio: 0.2,
                },
            },
            ..SimConfigFile::default()
        };

        let text = toml::to_string(&cfg).expect("serializes");
        let reparsed: SimConfigFile = toml::from_str(&text).expect("reparses");
        assert_eq!(cfg, reparsed);
    }

    #[test]
    fn round_trips_netrace_and_synfull_traffic_kinds() {
        for kind in [
            TrafficKind::Netrace {
                path: "trace.tni".to_string(),
                region: Some((0, 1000)),
                node_mapping: NodeMappingCfg::Modulo,
            },
            TrafficKind::SynFull {
                model_path: "model.toml".to_string(),
            },
        ] {
            let cfg = SimConfigFile {
                traffic: TrafficCfg {
                    kind,
                    packet_size: PacketSizeCfg::Fixed { size: 4 },
                },
                ..SimConfigFile::default()
            };
            let text = toml::to_string(&cfg).expect("serializes");
            let reparsed: SimConfigFile = toml::from_str(&text).expect("reparses");
            assert_eq!(cfg, reparsed);
        }
    }
}
