use std::fs;
use std::io;

use tempfile::TempDir;
use zerovdd_config::io::save_atomic_with_pre_commit;
use zerovdd_config::{
    default_config_dir, default_config_path, load_from_path, load_or_default, save_atomic,
    BitDepthConfig, ColorSpaceConfig, ConfigError, ConfigV1, GpuSelector, HdrDefaults, LogLevel,
    LoggingConfig, PersistentDisplayConfig, SunshineConfig, SunshineTopologyMode,
    CURRENT_SCHEMA_VERSION,
};

#[test]
fn default_config_is_valid_and_roundtrips_toml() {
    let cfg = ConfigV1::default();
    assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(cfg.gpu, GpuSelector::AutoBest);
    assert!(cfg.headless_standby);
    assert!(cfg.persistent_displays.is_empty());
    assert_eq!(cfg.sunshine.topology_mode, SunshineTopologyMode::Isolate);
    cfg.validate().unwrap();

    let toml_text = cfg.to_toml_string().unwrap();
    let parsed = ConfigV1::from_toml_str(&toml_text).unwrap();
    assert_eq!(parsed, cfg);

    let explicit_false = ConfigV1::from_toml_str("schema_version = 1\nheadless_standby = false\n")
        .expect("explicit headless_standby=false parses");
    assert!(!explicit_false.headless_standby);
}

#[test]
fn roundtrips_all_gpu_selectors_and_populated_fields() {
    let selectors = [
        GpuSelector::AutoBest,
        GpuSelector::Luid {
            low: 0xDEAD_BEEF,
            high: 1,
        },
        GpuSelector::PciBus {
            bus: 1,
            device: 0,
            function: 0,
        },
        GpuSelector::FriendlyName {
            name: "NVIDIA GeForce RTX 4090".to_string(),
        },
    ];

    for (idx, selector) in selectors.into_iter().enumerate() {
        let cfg = ConfigV1 {
            schema_version: CURRENT_SCHEMA_VERSION,
            gpu: selector,
            headless_standby: idx % 2 == 0,
            persistent_displays: vec![PersistentDisplayConfig {
                name: "Headless-Primary".to_string(),
                width: 2560,
                height: 1440,
                refresh_mhz: 119_880,
                hdr: true,
                ..Default::default()
            }],
            hdr_defaults: HdrDefaults {
                enabled: true,
                max_luminance_nits: 1200,
                max_fall_nits: 600,
                min_luminance_ten_thousandths: 100,
            },
            sunshine: SunshineConfig {
                topology_mode: SunshineTopologyMode::Extend,
                default_width: 3840,
                default_height: 2160,
                default_refresh_mhz: 120_000,
                default_hdr: true,
                restore_on_exit: true,
                sunshine_config_path: Some(
                    r"C:\Program Files\Sunshine\config\sunshine.conf".into(),
                ),
                default_edid_path: Some(r"C:\ProgramData\zerovdd\edid.bin".into()),
            },
            logging: LoggingConfig {
                level: LogLevel::Debug,
                etw_enabled: true,
            },
        };

        let encoded = cfg.to_toml_string().unwrap();
        let decoded = ConfigV1::from_toml_str(&encoded).unwrap();
        assert_eq!(decoded, cfg);
    }
}

#[test]
fn strictly_rejects_unknown_fields_and_wrong_schema_version() {
    let unknown_root = r#"
schema_version = 1
unexpected_flag = true
"#;
    assert!(matches!(
        ConfigV1::from_toml_str(unknown_root),
        Err(ConfigError::TomlDeserialize(_))
    ));

    let unknown_nested = r#"
schema_version = 1
[sunshine]
topology_mode = "isolate"
default_width = 1920
default_height = 1080
default_refresh_mhz = 60000
default_hdr = false
restore_on_exit = true
typo_field = 123
"#;
    assert!(matches!(
        ConfigV1::from_toml_str(unknown_nested),
        Err(ConfigError::TomlDeserialize(_))
    ));

    let unknown_gpu_field = r#"
schema_version = 1
[gpu]
mode = "auto_best"
extra = "bad"
"#;
    assert!(matches!(
        ConfigV1::from_toml_str(unknown_gpu_field),
        Err(ConfigError::TomlDeserialize(_))
    ));

    let wrong_version = r#"
schema_version = 2
"#;
    assert!(matches!(
        ConfigV1::from_toml_str(wrong_version),
        Err(ConfigError::UnsupportedSchemaVersion {
            expected: 1,
            actual: 2
        })
    ));

    let missing_version = r#"
headless_standby = true
"#;
    assert!(matches!(
        ConfigV1::from_toml_str(missing_version),
        Err(ConfigError::TomlDeserialize(_))
    ));
}

#[test]
fn partial_toml_sections_fill_defaults_and_reject_unknown_fields() {
    let partial_toml = r#"
schema_version = 1

[sunshine]
topology_mode = "extend"

[hdr_defaults]
enabled = true

[logging]
level = "debug"
"#;
    let parsed = ConfigV1::from_toml_str(partial_toml).unwrap();
    assert!(parsed.headless_standby);
    assert_eq!(parsed.sunshine.topology_mode, SunshineTopologyMode::Extend);
    assert_eq!(parsed.sunshine.default_width, 1920);
    assert_eq!(parsed.sunshine.default_height, 1080);
    assert_eq!(parsed.sunshine.default_refresh_mhz, 60_000);
    assert!(parsed.hdr_defaults.enabled);
    assert_eq!(parsed.hdr_defaults.max_luminance_nits, 1000);
    assert_eq!(parsed.logging.level, LogLevel::Debug);
    assert!(parsed.logging.etw_enabled);

    let partial_unknown = r#"
schema_version = 1

[sunshine]
topology_mode = "extend"
bogus_key = true
"#;
    assert!(matches!(
        ConfigV1::from_toml_str(partial_unknown),
        Err(ConfigError::TomlDeserialize(_))
    ));

    let partial_hdr_unknown = r#"
schema_version = 1

[hdr_defaults]
enabled = true
unknown_nit = 500
"#;
    assert!(matches!(
        ConfigV1::from_toml_str(partial_hdr_unknown),
        Err(ConfigError::TomlDeserialize(_))
    ));
}

#[test]
fn rejects_invalid_dimensions_refresh_names_and_hdr_defaults() {
    let mut bad_sunshine_dim = ConfigV1::default();
    bad_sunshine_dim.sunshine.default_width = 0;
    assert!(matches!(
        bad_sunshine_dim.validate(),
        Err(ConfigError::InvalidDimensions { .. })
    ));

    let mut bad_sunshine_refresh = ConfigV1::default();
    bad_sunshine_refresh.sunshine.default_refresh_mhz = 0;
    assert!(matches!(
        bad_sunshine_refresh.validate(),
        Err(ConfigError::InvalidRefreshMhz { .. })
    ));

    let mut bad_disp_dim = ConfigV1::default();
    bad_disp_dim
        .persistent_displays
        .push(PersistentDisplayConfig {
            name: "Main".to_string(),
            width: 300,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            ..Default::default()
        });
    assert!(matches!(
        bad_disp_dim.validate(),
        Err(ConfigError::InvalidDimensions { .. })
    ));

    let dup_names = ConfigV1 {
        persistent_displays: vec![
            PersistentDisplayConfig {
                name: "Deck".to_string(),
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                hdr: false,
                ..Default::default()
            },
            PersistentDisplayConfig {
                name: "Deck".to_string(),
                width: 2560,
                height: 1440,
                refresh_mhz: 120_000,
                hdr: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert!(matches!(
        dup_names.validate(),
        Err(ConfigError::DuplicateDisplayName { .. })
    ));

    let bad_gpu_name = ConfigV1 {
        gpu: GpuSelector::FriendlyName {
            name: "   ".to_string(),
        },
        ..Default::default()
    };
    assert!(matches!(
        bad_gpu_name.validate(),
        Err(ConfigError::InvalidGpuSelector)
    ));

    let ctrl_gpu_name = ConfigV1 {
        gpu: GpuSelector::FriendlyName {
            name: "RTX\n4090".to_string(),
        },
        ..Default::default()
    };
    assert!(matches!(
        ctrl_gpu_name.validate(),
        Err(ConfigError::InvalidGpuSelector)
    ));

    let mut bad_hdr = ConfigV1::default();
    bad_hdr.hdr_defaults.max_fall_nits = 2000;
    bad_hdr.hdr_defaults.max_luminance_nits = 1000;
    assert!(matches!(
        bad_hdr.validate(),
        Err(ConfigError::InvalidHdrDefaults { .. })
    ));

    let mut empty_sunshine_path = ConfigV1::default();
    empty_sunshine_path.sunshine.sunshine_config_path = Some("  ".to_string());
    assert!(matches!(
        empty_sunshine_path.validate(),
        Err(ConfigError::EmptySunshineConfigPath)
    ));
}

#[test]
fn load_or_default_and_save_atomic_preserve_valid_config_on_failure() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("nested").join("config.toml");

    let initial = load_or_default(&cfg_path).unwrap();
    assert_eq!(initial, ConfigV1::default());
    assert!(!cfg_path.exists());

    let valid_v1 = ConfigV1 {
        headless_standby: true,
        sunshine: SunshineConfig {
            default_width: 2560,
            default_height: 1600,
            ..Default::default()
        },
        ..Default::default()
    };
    save_atomic(&cfg_path, &valid_v1).unwrap();

    let loaded = load_from_path(&cfg_path).unwrap();
    assert_eq!(loaded, valid_v1);

    let mut invalid_cfg = ConfigV1::default();
    invalid_cfg.sunshine.default_refresh_mhz = 0;
    assert!(save_atomic(&cfg_path, &invalid_cfg).is_err());
    assert_eq!(load_from_path(&cfg_path).unwrap(), valid_v1);

    let mut next_valid = valid_v1.clone();
    next_valid.sunshine.default_width = 3840;
    let interrupted = save_atomic_with_pre_commit(&cfg_path, &next_valid, || {
        Err(io::Error::other("simulated power loss before rename"))
    });
    assert!(interrupted.is_err());

    // WHY: Verify the prior valid file remained completely intact and no leftover temp files remain.
    assert_eq!(load_from_path(&cfg_path).unwrap(), valid_v1);
    let entries: Vec<_> = fs::read_dir(cfg_path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "config.toml");

    fs::write(&cfg_path, "schema_version = 1\nbad_key = 42\n").unwrap();
    assert!(matches!(
        load_or_default(&cfg_path),
        Err(ConfigError::TomlDeserialize(_))
    ));
}

#[test]
fn default_config_paths_have_expected_components() {
    let dir = default_config_dir();
    assert!(dir.ends_with("zerovdd"));
    let path = default_config_path();
    assert!(path.ends_with("config.toml"));
    assert_eq!(path.parent(), Some(dir.as_path()));
}

#[test]
fn corpus_regression_replay() {
    let unknown_field = include_str!("../fixtures/corpus_unknown_field.toml");
    assert!(matches!(
        ConfigV1::from_toml_str(unknown_field),
        Err(ConfigError::TomlDeserialize(_))
    ));

    let wrong_version = include_str!("../fixtures/corpus_wrong_version.toml");
    assert!(matches!(
        ConfigV1::from_toml_str(wrong_version),
        Err(ConfigError::UnsupportedSchemaVersion {
            expected: CURRENT_SCHEMA_VERSION,
            actual: 99
        })
    ));

    let invalid_hdr = include_str!("../fixtures/corpus_invalid_hdr.toml");
    assert!(matches!(
        ConfigV1::from_toml_str(invalid_hdr),
        Err(ConfigError::InvalidHdrDefaults { .. })
    ));

    let valid_partial = include_str!("../fixtures/corpus_valid_partial.toml");
    let parsed = ConfigV1::from_toml_str(valid_partial).expect("valid partial config must parse");
    parsed.validate().unwrap();
    assert!(parsed.headless_standby);
    assert_eq!(parsed.persistent_displays.len(), 1);
    assert_eq!(parsed.persistent_displays[0].name, "Desk-4K");
    let res = parsed.to_toml_string().unwrap();
    assert_eq!(ConfigV1::from_toml_str(&res).unwrap(), parsed);
}

#[test]
fn persistent_display_color_ppi_and_hdr_nits_roundtrip_and_validation() {
    let raw = r#"
schema_version = 1

[[persistent_displays]]
name = "Studio-P3"
width = 3840
height = 2160
refresh_mhz = 120000
hdr = true
color_space = "dci-p3"
bit_depth = "10"
target_ppi = 140
max_luminance_nits = 1600
max_fall_nits = 500
min_luminance_ten_thousandths = 250
"#;
    let cfg = ConfigV1::from_toml_str(raw).unwrap();
    assert_eq!(
        cfg.persistent_displays[0].color_space,
        ColorSpaceConfig::DciP3
    );
    assert_eq!(cfg.persistent_displays[0].bit_depth, BitDepthConfig::Bpc10);
    assert_eq!(cfg.persistent_displays[0].target_ppi, Some(140));
    assert_eq!(cfg.persistent_displays[0].max_luminance_nits, Some(1600));

    let mut bad_ppi = cfg.clone();
    bad_ppi.persistent_displays[0].target_ppi = Some(47);
    assert!(matches!(
        bad_ppi.validate(),
        Err(ConfigError::InvalidTargetPpi { target_ppi: 47, .. })
    ));
    bad_ppi.persistent_displays[0].target_ppi = Some(601);
    assert!(matches!(
        bad_ppi.validate(),
        Err(ConfigError::InvalidTargetPpi {
            target_ppi: 601,
            ..
        })
    ));

    let mut bad_bpc = cfg.clone();
    bad_bpc.persistent_displays[0].bit_depth = BitDepthConfig::Bpc8;
    assert!(matches!(
        bad_bpc.validate(),
        Err(ConfigError::IncompatibleColorSettings { .. })
    ));

    let mut bad_sdr_p3 = cfg.clone();
    bad_sdr_p3.persistent_displays[0].hdr = false;
    assert!(matches!(
        bad_sdr_p3.validate(),
        Err(ConfigError::IncompatibleColorSettings { .. })
    ));

    let mut bad_nits = cfg.clone();
    bad_nits.persistent_displays[0].max_fall_nits = Some(2000);
    assert!(matches!(
        bad_nits.validate(),
        Err(ConfigError::InvalidHdrDefaults { .. })
    ));

    let mut with_edid = cfg;
    with_edid.persistent_displays[0].edid_path = Some(r"C:\ProgramData\zerovdd\custom.edid".into());
    with_edid.sunshine.default_edid_path = Some(r"C:\ProgramData\zerovdd\sunshine.edid".into());
    let encoded_edid = with_edid.to_toml_string().unwrap();
    assert_eq!(ConfigV1::from_toml_str(&encoded_edid).unwrap(), with_edid);

    let mut empty_disp_edid = with_edid.clone();
    empty_disp_edid.persistent_displays[0].edid_path = Some("   ".into());
    assert!(matches!(
        empty_disp_edid.validate(),
        Err(ConfigError::EmptyEdidPath { .. })
    ));

    let mut empty_sunshine_edid = with_edid;
    empty_sunshine_edid.sunshine.default_edid_path = Some(String::new());
    assert!(matches!(
        empty_sunshine_edid.validate(),
        Err(ConfigError::EmptyEdidPath { .. })
    ));
}
