//! Strongly typed TOML configuration schema and atomic persistence for `zerovdd`.

#![forbid(unsafe_code)]

/// Atomic configuration file loading and persistence helpers.
pub mod io;
/// Versioned configuration schema, validation, and TOML codecs.
pub mod schema;

pub use io::{
    default_config_dir, default_config_path, load_from_path, load_or_default, save_atomic,
};
pub use schema::{
    BitDepthConfig, ColorSpaceConfig, ConfigError, ConfigV1, GpuSelector, HdrDefaults, LogLevel,
    LoggingConfig, PersistentDisplayConfig, SunshineConfig, SunshineTopologyMode,
    CURRENT_SCHEMA_VERSION, MAX_TARGET_PPI, MIN_TARGET_PPI,
};
