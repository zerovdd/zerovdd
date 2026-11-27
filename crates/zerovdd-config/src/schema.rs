//! Versioned TOML configuration schema and semantic validation for `zerovdd`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Current configuration schema version supported by [`ConfigV1`].
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Minimum allowed configured display width or height in pixels.
pub const MIN_DISPLAY_DIM: u32 = 320;

/// Maximum allowed configured display width or height in pixels.
pub const MAX_DISPLAY_DIM: u32 = 16_384;

/// Minimum allowed configured refresh rate in millihertz (10 Hz).
pub const MIN_REFRESH_MHZ: u32 = 10_000;

/// Maximum allowed configured refresh rate in millihertz (1000 Hz).
pub const MAX_REFRESH_MHZ: u32 = 1_000_000;

/// Maximum number of persistent displays allowed in configuration.
pub const MAX_PERSISTENT_DISPLAYS: usize = 16;

/// Maximum byte length allowed for a persistent display name.
pub const MAX_DISPLAY_NAME_LEN: usize = 64;

/// Maximum allowed HDR peak mastering luminance in nits.
pub const MAX_HDR_LUMINANCE_NITS: u16 = 10_000;

/// Minimum allowed explicit virtual display pixel density in PPI.
pub const MIN_TARGET_PPI: u16 = 48;

/// Maximum allowed explicit virtual display pixel density in PPI.
pub const MAX_TARGET_PPI: u16 = 600;

/// Errors produced when loading, parsing, validating, or saving configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Configuration schema version is unsupported.
    #[error("unsupported config schema_version {actual}; expected {expected}")]
    UnsupportedSchemaVersion {
        /// Expected schema version.
        expected: u32,
        /// Found schema version.
        actual: u32,
    },

    /// Display dimensions are outside supported bounds.
    #[error("invalid dimensions {width}x{height} in `{context}`")]
    InvalidDimensions {
        /// Config section or display name where the invalid value occurred.
        context: String,
        /// Configured width in pixels.
        width: u32,
        /// Configured height in pixels.
        height: u32,
    },

    /// Refresh rate in millihertz is zero or outside supported bounds.
    #[error("invalid refresh_mhz {refresh_mhz} in `{context}`")]
    InvalidRefreshMhz {
        /// Config section or display name where the invalid value occurred.
        context: String,
        /// Configured refresh rate in millihertz.
        refresh_mhz: u32,
    },

    /// Persistent display entry name is empty, too long, or contains control characters.
    #[error("invalid persistent display name `{name}`")]
    InvalidDisplayName {
        /// Invalid display name string.
        name: String,
    },

    /// Duplicate persistent display name was configured.
    #[error("duplicate persistent display name `{name}`")]
    DuplicateDisplayName {
        /// Duplicated display name.
        name: String,
    },

    /// Too many persistent displays were configured.
    #[error("too many persistent displays ({count}); maximum is {MAX_PERSISTENT_DISPLAYS}")]
    TooManyPersistentDisplays {
        /// Configured count.
        count: usize,
    },

    /// GPU selector friendly name is empty or exceeds maximum length.
    #[error("invalid GPU friendly name")]
    InvalidGpuSelector,

    /// HDR luminance defaults are outside valid CTA-861-G bounds or inconsistent.
    #[error("invalid HDR defaults (max_luminance_nits={max_nits}, max_fall_nits={max_fall_nits})")]
    InvalidHdrDefaults {
        /// Configured peak luminance in nits.
        max_nits: u16,
        /// Configured maximum frame-average light level in nits.
        max_fall_nits: u16,
    },

    /// Persistent display target PPI is outside supported bounds (`48..=600`).
    #[error("invalid target_ppi {target_ppi} in `{context}`: expected {MIN_TARGET_PPI}..={MAX_TARGET_PPI}")]
    InvalidTargetPpi {
        /// Display name where the invalid value occurred.
        context: String,
        /// Configured pixel density.
        target_ppi: u16,
    },

    /// Persistent display color space or bit depth is incompatible with its `hdr` setting.
    #[error("incompatible color settings for `{context}` (hdr={hdr}, color_space={color_space:?}, bit_depth={bit_depth:?})")]
    IncompatibleColorSettings {
        /// Display name where the mismatch occurred.
        context: String,
        /// Whether HDR was enabled.
        hdr: bool,
        /// Configured color space preference.
        color_space: ColorSpaceConfig,
        /// Configured bit-depth preference.
        bit_depth: BitDepthConfig,
    },

    /// Sunshine config path override was provided as an empty/whitespace string.
    #[error("sunshine_config_path must not be empty when specified")]
    EmptySunshineConfigPath,

    /// EDID file path override was provided as an empty/whitespace string.
    #[error("edid path in `{context}` must not be empty when specified")]
    EmptyEdidPath {
        /// Config section or display name where the empty path occurred.
        context: String,
    },

    /// Persistent display orientation is not one of `0`, `90`, `180`, or `270` degrees.
    #[error(
        "invalid display orientation {orientation} in `{context}`: expected 0, 90, 180, or 270"
    )]
    InvalidOrientation {
        /// Config section or display name where the invalid orientation occurred.
        context: String,
        /// Configured clockwise orientation in degrees.
        orientation: u16,
    },

    /// TOML deserialization error (including unknown or mismatched fields).
    #[error("failed to parse TOML configuration: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

    /// TOML serialization error.
    #[error("failed to serialize TOML configuration: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    /// Filesystem I/O error while loading or atomically saving configuration.
    #[error("config I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Target config path has no parent directory or cannot be persisted atomically.
    #[error("invalid config file path `{0}`")]
    InvalidPath(String),
}

/// Strategy for selecting which physical GPU renders virtual display frames.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum GpuSelector {
    /// Automatically pick the non-WARP adapter with highest dedicated video memory.
    #[default]
    AutoBest,
    /// Select a specific adapter by Windows `LUID` (`LowPart`, `HighPart`).
    Luid {
        /// Low 32 bits of the adapter LUID.
        low: u32,
        /// High 32 bits of the adapter LUID.
        high: i32,
    },
    /// Select a specific adapter by PCI bus location.
    PciBus {
        /// PCI bus number.
        bus: u32,
        /// PCI device number.
        device: u32,
        /// PCI function number.
        function: u32,
    },
    /// Select a specific adapter by substring/exact DXGI description match.
    FriendlyName {
        /// Adapter friendly name string.
        name: String,
    },
}

// WHY: Serde's internally-tagged unit variants ignore extra keys even with deny_unknown_fields;
// deserializing via an empty struct variant enforces strict unknown-field rejection for AutoBest.
#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum GpuSelectorWire {
    AutoBest {},
    Luid {
        low: u32,
        high: i32,
    },
    PciBus {
        bus: u32,
        device: u32,
        function: u32,
    },
    FriendlyName {
        name: String,
    },
}

impl<'de> Deserialize<'de> for GpuSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match GpuSelectorWire::deserialize(deserializer)? {
            GpuSelectorWire::AutoBest {} => Ok(Self::AutoBest),
            GpuSelectorWire::Luid { low, high } => Ok(Self::Luid { low, high }),
            GpuSelectorWire::PciBus {
                bus,
                device,
                function,
            } => Ok(Self::PciBus {
                bus,
                device,
                function,
            }),
            GpuSelectorWire::FriendlyName { name } => Ok(Self::FriendlyName { name }),
        }
    }
}

/// Color space preference for persistent virtual displays in `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpaceConfig {
    /// Automatically choose BT.709 for SDR or BT.2020 for HDR10.
    #[default]
    Auto,
    /// ITU-R BT.709 / sRGB primaries (SDR).
    Bt709,
    /// ITU-R BT.2020 wide color gamut primaries (HDR10).
    Bt2020,
    /// Display P3 / DCI-P3 primaries (HDR10).
    #[serde(alias = "dci-p3")]
    DciP3,
}

/// Bit-depth cap per color component for persistent virtual displays in `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BitDepthConfig {
    /// Automatic defaults (`8|10` bpc for SDR, `10|12` bpc for HDR10).
    #[default]
    Auto,
    /// Restrict output modes to 8 bpc (SDR).
    #[serde(alias = "8")]
    Bpc8,
    /// Restrict output modes to 10 bpc.
    #[serde(alias = "10")]
    Bpc10,
    /// Restrict output modes to 12 bpc.
    #[serde(alias = "12")]
    Bpc12,
}

/// Configuration for a persistent virtual display maintained by `zerovdd-svc`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistentDisplayConfig {
    /// Unique stable name used to derive the monitor container GUID and EDID label.
    pub name: String,
    /// Display width in pixels.
    pub width: u32,
    /// Display height in pixels.
    pub height: u32,
    /// Refresh rate in millihertz (e.g. `60_000` for 60 Hz, `59_940` for 59.94 Hz).
    pub refresh_mhz: u32,
    /// Whether HDR10 capability is enabled for this persistent display.
    #[serde(default)]
    pub hdr: bool,
    /// Explicit color space preference (`auto`, `bt709`, `bt2020`, `dci_p3`).
    #[serde(default)]
    pub color_space: ColorSpaceConfig,
    /// Explicit component bit-depth cap (`auto`, `bpc8`/`8`, `bpc10`/`10`, `bpc12`/`12`).
    #[serde(default)]
    pub bit_depth: BitDepthConfig,
    /// Optional target pixel density in PPI (`48..=600`).
    #[serde(default)]
    pub target_ppi: Option<u16>,
    /// Optional peak HDR10 mastering luminance override in nits.
    #[serde(default)]
    pub max_luminance_nits: Option<u16>,
    /// Optional maximum frame-average light level override in nits.
    #[serde(default)]
    pub max_fall_nits: Option<u16>,
    /// Optional minimum mastering luminance override in `0.0001`-nit units.
    #[serde(default)]
    pub min_luminance_ten_thousandths: Option<u32>,
    /// Optional path to a custom EDID binary file (`128..=512` bytes, multiple of 128).
    #[serde(default)]
    pub edid_path: Option<String>,
    /// Optional clockwise rotation in degrees (`0`, `90`, `180`, `270`).
    #[serde(default)]
    pub orientation: Option<u16>,
    /// Optional desktop origin `(x, y)` in pixels when extending.
    #[serde(default)]
    pub position: Option<(i32, i32)>,
    /// Whether this virtual display should become the desktop primary `(0, 0)`.
    #[serde(default)]
    pub primary: bool,
}

/// Default HDR10 capability and mastering metadata settings for virtual displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HdrDefaults {
    /// Whether HDR is enabled by default when callers do not specify an override.
    pub enabled: bool,
    /// Peak mastering luminance in nits (`cd/m^2`).
    pub max_luminance_nits: u16,
    /// Maximum Frame-Average Light Level in nits.
    pub max_fall_nits: u16,
    /// Minimum mastering luminance in 0.0001-nit units (e.g. `500` = `0.05` nits).
    pub min_luminance_ten_thousandths: u32,
}

impl Default for HdrDefaults {
    fn default() -> Self {
        Self {
            enabled: false,
            max_luminance_nits: 1000,
            max_fall_nits: 400,
            min_luminance_ten_thousandths: 500,
        }
    }
}

/// Display topology mode applied when a Sunshine streaming session starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SunshineTopologyMode {
    /// Temporarily disable physical monitors and make the virtual display sole primary.
    #[default]
    Isolate,
    /// Keep physical monitors active and extend the desktop with the virtual display.
    Extend,
}

/// Sunshine integration defaults and behavior flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SunshineConfig {
    /// Desktop topology mode used during streaming sessions (`isolate` vs `extend`).
    pub topology_mode: SunshineTopologyMode,
    /// Fallback display width when `SUNSHINE_CLIENT_WIDTH` is absent.
    pub default_width: u32,
    /// Fallback display height when `SUNSHINE_CLIENT_HEIGHT` is absent.
    pub default_height: u32,
    /// Fallback refresh rate in millihertz when `SUNSHINE_CLIENT_FPS` is absent.
    pub default_refresh_mhz: u32,
    /// Fallback HDR flag when `SUNSHINE_CLIENT_HDR` is absent.
    pub default_hdr: bool,
    /// Whether physical display topology should be restored when streaming ends or crashes.
    pub restore_on_exit: bool,
    /// Optional explicit path to `sunshine.conf` (overrides standard auto-discovery).
    #[serde(default)]
    pub sunshine_config_path: Option<String>,
    /// Optional path to a default custom EDID binary file for Sunshine sessions.
    #[serde(default)]
    pub default_edid_path: Option<String>,
}

impl Default for SunshineConfig {
    fn default() -> Self {
        Self {
            topology_mode: SunshineTopologyMode::Isolate,
            default_width: 1920,
            default_height: 1080,
            default_refresh_mhz: 60_000,
            default_hdr: false,
            restore_on_exit: true,
            sunshine_config_path: None,
            default_edid_path: None,
        }
    }
}

/// Verbosity filter for user-mode CLI/service logs and driver telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Log only unrecoverable errors.
    Error,
    /// Log warnings and errors.
    Warn,
    /// Log standard operational events, warnings, and errors.
    #[default]
    Info,
    /// Log detailed diagnostic events.
    Debug,
    /// Log verbose trace diagnostics.
    Trace,
}

/// Logging and ETW TraceLogging settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// Minimum verbosity level emitted by `zerovdd` components.
    pub level: LogLevel,
    /// Whether structured ETW TraceLogging events are enabled.
    pub etw_enabled: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            etw_enabled: true,
        }
    }
}

const fn default_true() -> bool {
    true
}

/// Versioned root configuration for `zerovdd` (`schema_version = 1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigV1 {
    /// Configuration schema version (must equal [`CURRENT_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Preferred GPU adapter selector for driver rendering.
    #[serde(default)]
    pub gpu: GpuSelector,
    /// Keep a headless standby virtual display attached when no physical monitors exist.
    #[serde(default = "default_true")]
    pub headless_standby: bool,
    /// Optional list of service-managed persistent virtual displays.
    #[serde(default)]
    pub persistent_displays: Vec<PersistentDisplayConfig>,
    /// Default HDR10 capability and mastering metadata.
    #[serde(default)]
    pub hdr_defaults: HdrDefaults,
    /// Sunshine integration defaults and topology preferences.
    #[serde(default)]
    pub sunshine: SunshineConfig,
    /// Logging and ETW configuration.
    #[serde(default)]
    pub logging: LoggingConfig,
}

impl Default for ConfigV1 {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            gpu: GpuSelector::default(),
            headless_standby: true,
            persistent_displays: Vec::new(),
            hdr_defaults: HdrDefaults::default(),
            sunshine: SunshineConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl ConfigV1 {
    /// Parses a TOML configuration string with strict field checking and semantic validation.
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(input)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validates and serializes this configuration to a pretty-printed TOML string.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        self.validate()?;
        Ok(toml::to_string_pretty(self)?)
    }

    /// Performs semantic validation across all configuration fields.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchemaVersion {
                expected: CURRENT_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }

        if let GpuSelector::FriendlyName { ref name } = self.gpu {
            if name.trim().is_empty()
                || name.len() > 128
                || name.bytes().any(|b| b < 0x20 || b == 0x7F)
            {
                return Err(ConfigError::InvalidGpuSelector);
            }
        }

        if self.persistent_displays.len() > MAX_PERSISTENT_DISPLAYS {
            return Err(ConfigError::TooManyPersistentDisplays {
                count: self.persistent_displays.len(),
            });
        }

        let mut seen_names = BTreeSet::new();
        for disp in &self.persistent_displays {
            let trimmed = disp.name.trim();
            if trimmed.is_empty()
                || disp.name.len() > MAX_DISPLAY_NAME_LEN
                || disp.name.bytes().any(|b| b < 0x20 || b == 0x7F)
            {
                return Err(ConfigError::InvalidDisplayName {
                    name: disp.name.clone(),
                });
            }
            if !seen_names.insert(trimmed.to_string()) {
                return Err(ConfigError::DuplicateDisplayName {
                    name: disp.name.clone(),
                });
            }
            validate_dimensions(&disp.name, disp.width, disp.height)?;
            validate_refresh_mhz(&disp.name, disp.refresh_mhz)?;
            if let Some(ppi) = disp.target_ppi {
                if !(MIN_TARGET_PPI..=MAX_TARGET_PPI).contains(&ppi) {
                    return Err(ConfigError::InvalidTargetPpi {
                        context: disp.name.clone(),
                        target_ppi: ppi,
                    });
                }
            }
            let compat_color = match (disp.hdr, disp.color_space) {
                (_, ColorSpaceConfig::Auto)
                | (false, ColorSpaceConfig::Bt709)
                | (true, ColorSpaceConfig::Bt2020 | ColorSpaceConfig::DciP3) => true,
                (false, ColorSpaceConfig::Bt2020 | ColorSpaceConfig::DciP3)
                | (true, ColorSpaceConfig::Bt709) => false,
            };
            let compat_depth = !(disp.hdr && disp.bit_depth == BitDepthConfig::Bpc8);
            if !compat_color || !compat_depth {
                return Err(ConfigError::IncompatibleColorSettings {
                    context: disp.name.clone(),
                    hdr: disp.hdr,
                    color_space: disp.color_space,
                    bit_depth: disp.bit_depth,
                });
            }
            if disp.max_luminance_nits.is_some()
                || disp.max_fall_nits.is_some()
                || disp.min_luminance_ten_thousandths.is_some()
            {
                let eff_max = disp
                    .max_luminance_nits
                    .unwrap_or(self.hdr_defaults.max_luminance_nits);
                let eff_fall = disp
                    .max_fall_nits
                    .unwrap_or(self.hdr_defaults.max_fall_nits.min(eff_max));
                let eff_min = disp
                    .min_luminance_ten_thousandths
                    .unwrap_or(self.hdr_defaults.min_luminance_ten_thousandths);
                if !(100..=MAX_HDR_LUMINANCE_NITS).contains(&eff_max)
                    || eff_fall == 0
                    || eff_fall > eff_max
                    || eff_min > u32::from(eff_max) * 10_000
                {
                    return Err(ConfigError::InvalidHdrDefaults {
                        max_nits: eff_max,
                        max_fall_nits: eff_fall,
                    });
                }
            }
            if let Some(ref edid_path) = disp.edid_path {
                if edid_path.trim().is_empty() {
                    return Err(ConfigError::EmptyEdidPath {
                        context: disp.name.clone(),
                    });
                }
            }
            if let Some(orientation) = disp.orientation {
                if !matches!(orientation, 0 | 90 | 180 | 270) {
                    return Err(ConfigError::InvalidOrientation {
                        context: disp.name.clone(),
                        orientation,
                    });
                }
            }
        }

        if self.hdr_defaults.max_luminance_nits < 100
            || self.hdr_defaults.max_luminance_nits > MAX_HDR_LUMINANCE_NITS
            || self.hdr_defaults.max_fall_nits == 0
            || self.hdr_defaults.max_fall_nits > self.hdr_defaults.max_luminance_nits
        {
            return Err(ConfigError::InvalidHdrDefaults {
                max_nits: self.hdr_defaults.max_luminance_nits,
                max_fall_nits: self.hdr_defaults.max_fall_nits,
            });
        }

        validate_dimensions(
            "sunshine",
            self.sunshine.default_width,
            self.sunshine.default_height,
        )?;
        validate_refresh_mhz("sunshine", self.sunshine.default_refresh_mhz)?;

        if let Some(ref path) = self.sunshine.sunshine_config_path {
            if path.trim().is_empty() {
                return Err(ConfigError::EmptySunshineConfigPath);
            }
        }
        if let Some(ref path) = self.sunshine.default_edid_path {
            if path.trim().is_empty() {
                return Err(ConfigError::EmptyEdidPath {
                    context: "sunshine.default_edid_path".to_string(),
                });
            }
        }

        Ok(())
    }
}

fn validate_dimensions(context: &str, width: u32, height: u32) -> Result<(), ConfigError> {
    if !(MIN_DISPLAY_DIM..=MAX_DISPLAY_DIM).contains(&width)
        || !(MIN_DISPLAY_DIM..=MAX_DISPLAY_DIM).contains(&height)
    {
        return Err(ConfigError::InvalidDimensions {
            context: context.to_string(),
            width,
            height,
        });
    }
    Ok(())
}

fn validate_refresh_mhz(context: &str, refresh_mhz: u32) -> Result<(), ConfigError> {
    if !(MIN_REFRESH_MHZ..=MAX_REFRESH_MHZ).contains(&refresh_mhz) {
        return Err(ConfigError::InvalidRefreshMhz {
            context: context.to_string(),
            refresh_mhz,
        });
    }
    Ok(())
}
