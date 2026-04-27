//! Pre-run validation (P1-F14): every rule below rejects an invalid config
//! with an actionable message (field, rule, fix) rather than letting a bad
//! run fail obscurely deep in the engine. All violations are collected in one
//! pass so a user fixes their config in one edit, not one error at a time.

use thiserror::Error;

use crate::schema::{is_adaptive_key, is_dimension_order_key, SimConfigFile};

#[derive(Clone, Debug, PartialEq, Error)]
pub enum ConfigError {
    #[error("version {found}: this build only understands schema version {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },

    #[error("router.num_vcs = {0}: must be in 1..=8")]
    VcCountOutOfRange(u8),

    #[error("router.buffer_depth = {0}: must be >= 2")]
    BufferTooShallow(u16),

    #[error("topology dimensions {width}x{height}: must be >= 2x2 (a single-router network has no links to route over)")]
    TopologyTooSmall { width: u16, height: u16 },

    #[error("traffic.kind = uniform: injection_rate {0} is out of range; must be in [0.0, 1.0]")]
    InjectionRateOutOfRange(f64),

    #[error("traffic.packet_size = bimodal: control size {control} must be <= data size {data}")]
    PacketSizeOrder { control: u16, data: u16 },

    #[error(
        "traffic.packet_size = bimodal: data_ratio {0} is out of range; must be in [0.0, 1.0]"
    )]
    DataRatioOutOfRange(f64),

    #[error(
        "router.routing = '{routing}' is fully adaptive, which needs router.vc_classes with \
         escape >= 1 and adaptive >= 1 (an escape-VC DOR fallback keeps it deadlock-free); \
         got {vc_classes:?}"
    )]
    AdaptiveNeedsEscapeVcs {
        routing: String,
        vc_classes: Option<(u8, u8)>,
    },

    #[error(
        "router.vc_classes = {{escape: {escape}, adaptive: {adaptive}}} sums to {sum}, but \
         router.num_vcs = {num_vcs}; the classes must exactly partition the available VCs"
    )]
    VcClassesDontPartition {
        escape: u8,
        adaptive: u8,
        sum: u16,
        num_vcs: u8,
    },

    #[error(
        "topology = torus2d with dimension-order routing ('{routing}') requires \
         router.dateline = true and router.num_vcs >= 2 (dateline VCs break the wraparound \
         cycle); got dateline={dateline}, num_vcs={num_vcs}"
    )]
    TorusDorNeedsDateline {
        routing: String,
        dateline: bool,
        num_vcs: u8,
    },
}

pub fn validate(cfg: &SimConfigFile) -> Result<(), Vec<ConfigError>> {
    let mut errors = Vec::new();

    if cfg.version != crate::schema::CURRENT_VERSION {
        errors.push(ConfigError::UnsupportedVersion {
            found: cfg.version,
            expected: crate::schema::CURRENT_VERSION,
        });
    }

    if cfg.router.num_vcs == 0 || cfg.router.num_vcs > 8 {
        errors.push(ConfigError::VcCountOutOfRange(cfg.router.num_vcs));
    }

    if cfg.router.buffer_depth < 2 {
        errors.push(ConfigError::BufferTooShallow(cfg.router.buffer_depth));
    }

    let (width, height) = cfg.topology.dimensions();
    if width < 2 || height < 2 {
        errors.push(ConfigError::TopologyTooSmall { width, height });
    }

    if let crate::schema::TrafficKind::Uniform { injection_rate } = cfg.traffic.kind {
        if !(0.0..=1.0).contains(&injection_rate) {
            errors.push(ConfigError::InjectionRateOutOfRange(injection_rate));
        }
    }

    if let crate::schema::PacketSizeCfg::Bimodal {
        control,
        data,
        data_ratio,
    } = cfg.traffic.packet_size
    {
        if control > data {
            errors.push(ConfigError::PacketSizeOrder { control, data });
        }
        if !(0.0..=1.0).contains(&data_ratio) {
            errors.push(ConfigError::DataRatioOutOfRange(data_ratio));
        }
    }

    if is_adaptive_key(&cfg.router.routing) {
        match cfg.router.vc_classes {
            Some(vc) if vc.escape >= 1 && vc.adaptive >= 1 => {}
            other => errors.push(ConfigError::AdaptiveNeedsEscapeVcs {
                routing: cfg.router.routing.clone(),
                vc_classes: other.map(|vc| (vc.escape, vc.adaptive)),
            }),
        }
    }

    if let Some(vc) = cfg.router.vc_classes {
        let sum = vc.escape as u16 + vc.adaptive as u16;
        if sum != cfg.router.num_vcs as u16 {
            errors.push(ConfigError::VcClassesDontPartition {
                escape: vc.escape,
                adaptive: vc.adaptive,
                sum,
                num_vcs: cfg.router.num_vcs,
            });
        }
    }

    if cfg.topology.is_torus()
        && is_dimension_order_key(&cfg.router.routing)
        && (!cfg.router.dateline || cfg.router.num_vcs < 2)
    {
        errors.push(ConfigError::TorusDorNeedsDateline {
            routing: cfg.router.routing.clone(),
            dateline: cfg.router.dateline,
            num_vcs: cfg.router.num_vcs,
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        AllocatorCfg, PacketSizeCfg, RouterCfg, TopologyCfg, TrafficCfg, TrafficKind, VcClassCfg,
    };

    fn good() -> SimConfigFile {
        SimConfigFile::default()
    }

    #[test]
    fn default_config_is_valid() {
        assert_eq!(validate(&good()), Ok(()));
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut cfg = good();
        cfg.version = 999;
        let err = validate(&cfg).unwrap_err();
        assert!(matches!(err[0], ConfigError::UnsupportedVersion { .. }));
    }

    #[test]
    fn rejects_vc_count_out_of_range() {
        let mut cfg = good();
        cfg.router.num_vcs = 0;
        assert!(validate(&cfg)
            .unwrap_err()
            .contains(&ConfigError::VcCountOutOfRange(0)));

        let mut cfg = good();
        cfg.router.num_vcs = 9;
        assert!(validate(&cfg)
            .unwrap_err()
            .contains(&ConfigError::VcCountOutOfRange(9)));
    }

    #[test]
    fn rejects_shallow_buffers() {
        let mut cfg = good();
        cfg.router.buffer_depth = 1;
        assert!(validate(&cfg)
            .unwrap_err()
            .contains(&ConfigError::BufferTooShallow(1)));
    }

    #[test]
    fn rejects_undersized_topology() {
        let mut cfg = good();
        cfg.topology = TopologyCfg::Mesh2D {
            width: 1,
            height: 4,
        };
        assert!(validate(&cfg)
            .unwrap_err()
            .contains(&ConfigError::TopologyTooSmall {
                width: 1,
                height: 4
            }));
    }

    #[test]
    fn rejects_out_of_range_injection_rate() {
        let mut cfg = good();
        cfg.traffic.kind = TrafficKind::Uniform {
            injection_rate: 1.5,
        };
        assert!(validate(&cfg)
            .unwrap_err()
            .contains(&ConfigError::InjectionRateOutOfRange(1.5)));
    }

    #[test]
    fn rejects_inverted_bimodal_packet_sizes() {
        let mut cfg = good();
        cfg.traffic.packet_size = PacketSizeCfg::Bimodal {
            control: 8,
            data: 4,
            data_ratio: 0.5,
        };
        assert!(validate(&cfg)
            .unwrap_err()
            .contains(&ConfigError::PacketSizeOrder {
                control: 8,
                data: 4
            }));
    }

    #[test]
    fn adaptive_routing_without_vc_classes_is_rejected() {
        let mut cfg = good();
        cfg.router.routing = "adaptive".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(matches!(err[0], ConfigError::AdaptiveNeedsEscapeVcs { .. }));
    }

    #[test]
    fn adaptive_routing_with_escape_vcs_is_accepted() {
        let mut cfg = good();
        cfg.router.routing = "adaptive".to_string();
        cfg.router.num_vcs = 2;
        cfg.router.vc_classes = Some(VcClassCfg {
            escape: 1,
            adaptive: 1,
        });
        assert_eq!(validate(&cfg), Ok(()));
    }

    #[test]
    fn vc_classes_must_partition_num_vcs() {
        let mut cfg = good();
        cfg.router.num_vcs = 4;
        cfg.router.vc_classes = Some(VcClassCfg {
            escape: 1,
            adaptive: 1,
        });
        assert!(validate(&cfg)
            .unwrap_err()
            .contains(&ConfigError::VcClassesDontPartition {
                escape: 1,
                adaptive: 1,
                sum: 2,
                num_vcs: 4
            }));
    }

    #[test]
    fn torus_dor_without_dateline_is_rejected() {
        let mut cfg = good();
        cfg.topology = TopologyCfg::Torus2D {
            width: 4,
            height: 4,
        };
        cfg.router.routing = "xy".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(matches!(err[0], ConfigError::TorusDorNeedsDateline { .. }));
    }

    #[test]
    fn torus_dor_with_dateline_and_two_vcs_is_accepted() {
        let mut cfg = good();
        cfg.topology = TopologyCfg::Torus2D {
            width: 4,
            height: 4,
        };
        cfg.router.routing = "xy".to_string();
        cfg.router.dateline = true;
        cfg.router.num_vcs = 2;
        assert_eq!(validate(&cfg), Ok(()));
    }

    #[test]
    fn collects_every_violation_in_one_pass() {
        let mut cfg = good();
        cfg.router.num_vcs = 0;
        cfg.router.buffer_depth = 1;
        cfg.topology = TopologyCfg::Mesh2D {
            width: 1,
            height: 1,
        };
        let errs = validate(&cfg).unwrap_err();
        assert_eq!(errs.len(), 3, "{errs:?}");
    }

    #[test]
    fn allocator_and_selection_defaults_are_unused_placeholders_for_now() {
        // Smoke-check the schema's remaining fields construct and serialize;
        // they gain validation rules once their engine hooks land (M2/M4).
        let cfg = RouterCfg {
            allocator_va: AllocatorCfg::ISlip,
            allocator_sa: AllocatorCfg::Matrix,
            alloc_iters: 2,
            ..good().router
        };
        let mut full = good();
        full.router = cfg;
        full.traffic = TrafficCfg {
            kind: full.traffic.kind,
            packet_size: PacketSizeCfg::Fixed { size: 4 },
        };
        assert_eq!(validate(&full), Ok(()));
    }
}
