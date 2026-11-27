//! Persistent virtual display profiles and headless-standby lifecycle management.

use std::path::Path;

use thiserror::Error;
use zerovdd_client::device::ControlClient;
use zerovdd_client::{monitor_guid_for_name, ClientError};
use zerovdd_config::{
    default_config_path, load_or_default, BitDepthConfig, ColorSpaceConfig, ConfigError, ConfigV1,
    GpuSelector, HdrDefaults, SunshineTopologyMode,
};
use zerovdd_proto::types::{
    AddDisplayRequest, BitDepthPreference, ColorSpacePreference, HdrMasteringMetadata, HdrProfile,
    OwnershipMode, RemoveDisplayRequest, SetRenderAdapterRequest, MAX_CUSTOM_EDID_LEN,
};
use zerovdd_proto::validation::ProtoError;

use crate::console_launcher::{ApplyCcdPayload, ConsoleCcdRunner, LauncherError};

/// Stable client identifier used for the optional headless-standby virtual display.
pub const HEADLESS_STANDBY_CLIENT_ID: &str = "zerovdd-headless-standby";

/// Standby virtual display width in pixels.
pub const HEADLESS_STANDBY_WIDTH: u32 = 1920;

/// Standby virtual display height in pixels.
pub const HEADLESS_STANDBY_HEIGHT: u32 = 1080;

/// Standby virtual display refresh rate in millihertz (60 Hz).
pub const HEADLESS_STANDBY_REFRESH_MHZ: u32 = 60_000;

/// Errors produced while loading configuration or reconciling persistent/standby displays.
#[derive(Debug, Error)]
pub enum PersistentError {
    /// Failed to load or validate `ConfigV1`.
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),

    /// Failed to construct a valid IOCTL payload.
    #[error("protocol payload error: {0}")]
    Proto(#[from] ProtoError),

    /// Control Device IOCTL failed.
    #[error("driver client error: {0}")]
    Client(#[from] ClientError),

    /// Console-session display probing failed.
    #[error("console probe error: {0}")]
    Launcher(#[from] LauncherError),
}

const fn gcd_u32(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Converts a millihertz refresh rate into an exact reduced `(numerator, denominator)` pair.
pub const fn millihz_to_rational(millihz: u32) -> (u32, u32) {
    match millihz {
        23_976 => (24_000, 1001),
        29_970 => (30_000, 1001),
        47_952 => (48_000, 1001),
        59_940 => (60_000, 1001),
        119_880 => (120_000, 1001),
        239_760 => (240_000, 1001),
        other => {
            let g = gcd_u32(other, 1000);
            (other / g, 1000 / g)
        }
    }
}

/// Builds [`HdrMasteringMetadata`] from [`HdrDefaults`].
pub const fn hdr_metadata_from_defaults(defaults: &HdrDefaults) -> HdrMasteringMetadata {
    HdrMasteringMetadata {
        max_luminance_nits: defaults.max_luminance_nits,
        max_fall_nits: defaults.max_fall_nits,
        min_luminance_ten_thousandths: defaults.min_luminance_ten_thousandths,
    }
}

/// Builds per-display [`HdrMasteringMetadata`] from optional overrides and [`HdrDefaults`].
pub fn hdr_metadata_with_overrides(
    defaults: &HdrDefaults,
    max_nits: Option<u16>,
    max_fall: Option<u16>,
    min_nits_10k: Option<u32>,
) -> HdrMasteringMetadata {
    let eff_max = max_nits.unwrap_or(defaults.max_luminance_nits);
    let eff_fall = max_fall.unwrap_or(defaults.max_fall_nits.min(eff_max));
    let eff_min = min_nits_10k.unwrap_or(defaults.min_luminance_ten_thousandths);
    HdrMasteringMetadata {
        max_luminance_nits: eff_max,
        max_fall_nits: eff_fall,
        min_luminance_ten_thousandths: eff_min,
    }
}

const fn map_color_space_config(cfg: ColorSpaceConfig) -> ColorSpacePreference {
    match cfg {
        ColorSpaceConfig::Auto => ColorSpacePreference::Auto,
        ColorSpaceConfig::Bt709 => ColorSpacePreference::Bt709,
        ColorSpaceConfig::Bt2020 => ColorSpacePreference::Bt2020,
        ColorSpaceConfig::DciP3 => ColorSpacePreference::DciP3,
    }
}

const fn map_bit_depth_config(cfg: BitDepthConfig) -> BitDepthPreference {
    match cfg {
        BitDepthConfig::Auto => BitDepthPreference::Auto,
        BitDepthConfig::Bpc8 => BitDepthPreference::Bpc8,
        BitDepthConfig::Bpc10 => BitDepthPreference::Bpc10,
        BitDepthConfig::Bpc12 => BitDepthPreference::Bpc12,
    }
}

/// Loads `ConfigV1` from `config_path` (or the platform default path when `None`).
pub fn load_service_config(config_path: Option<&Path>) -> Result<ConfigV1, PersistentError> {
    let default_path;
    let resolved = match config_path {
        Some(path) => path,
        None => {
            default_path = default_config_path();
            &default_path
        }
    };
    Ok(load_or_default(resolved)?)
}

/// Applies explicit LUID GPU preference if configured in `config.gpu`.
pub fn apply_configured_gpu_preference(
    driver: &dyn ControlClient,
    config: &ConfigV1,
) -> Result<(), PersistentError> {
    if let GpuSelector::Luid { low, high } = config.gpu {
        driver.set_render_adapter(&SetRenderAdapterRequest::new(low, high))?;
    }
    Ok(())
}

/// Reads and validates a custom EDID binary file (`128..=512` bytes, multiple of 128).
pub fn load_and_validate_edid_file(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path)
        .map_err(|err| format!("failed to read EDID file `{}`: {err}", path.display()))?;
    if bytes.len() > MAX_CUSTOM_EDID_LEN {
        return Err(format!(
            "EDID file `{}` is {} bytes (maximum is {MAX_CUSTOM_EDID_LEN} bytes)",
            path.display(),
            bytes.len()
        ));
    }
    zerovdd_edid::validate_custom_edid(&bytes)
        .map_err(|err| format!("invalid EDID in `{}`: {err}", path.display()))?;
    Ok(bytes)
}

/// Ensures every configured `persistent_displays[]` entry is attached via `OwnershipMode::HandleBound`.
pub fn reconcile_persistent_displays(
    driver: &dyn ControlClient,
    config: &ConfigV1,
) -> Result<Vec<[u8; 16]>, PersistentError> {
    let mut attached_guids = Vec::with_capacity(config.persistent_displays.len());
    for disp in &config.persistent_displays {
        let custom_edid = match disp.edid_path {
            Some(ref path_str) => match load_and_validate_edid_file(Path::new(path_str)) {
                Ok(bytes) => Some(bytes),
                Err(msg) => {
                    eprintln!(
                        "zerovdd-svc skipping persistent display `{}` due to custom EDID error: {msg}",
                        disp.name
                    );
                    continue;
                }
            },
            None => None,
        };
        let guid = monitor_guid_for_name(&disp.name);
        let (refresh_num, refresh_den) = millihz_to_rational(disp.refresh_mhz);
        let hdr_profile = if disp.hdr {
            HdrProfile::Hdr10
        } else {
            HdrProfile::Sdr
        };
        let mut req = AddDisplayRequest::new(
            guid,
            disp.width,
            disp.height,
            refresh_num,
            refresh_den,
            hdr_profile,
            OwnershipMode::HandleBound,
        );
        if disp.hdr {
            req.hdr_metadata = hdr_metadata_with_overrides(
                &config.hdr_defaults,
                disp.max_luminance_nits,
                disp.max_fall_nits,
                disp.min_luminance_ten_thousandths,
            );
        }
        req.target_ppi = disp.target_ppi.unwrap_or(0);
        req.color_space = map_color_space_config(disp.color_space).as_u8();
        req.bit_depth = map_bit_depth_config(disp.bit_depth).as_u8();
        if let Some(ref edid_bytes) = custom_edid {
            req.set_custom_edid(edid_bytes)?;
        }
        req.set_client_name(&disp.name)?;
        driver.add_display(&req)?;
        attached_guids.push(guid);
    }
    Ok(attached_guids)
}

/// Active headless-standby display identity and driver target coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandbyDisplay {
    /// Deterministic monitor GUID assigned to `zerovdd-headless-standby`.
    pub guid: [u8; 16],
    /// Driver `(adapter_luid, target_id)` returned by `AddDisplay` for filtering self during CCD probes.
    pub target: Option<(zerovdd_client::topology::AdapterLuid, u32)>,
}

/// Attaches a `1920x1080@60Hz` SDR standby display when `headless_standby` is enabled and no other
/// active display is present, or departs it automatically once another display becomes active.
pub fn reconcile_headless_standby(
    driver: &dyn ControlClient,
    launcher: &dyn ConsoleCcdRunner,
    config: &ConfigV1,
    standby: &mut Option<StandbyDisplay>,
) -> Result<bool, PersistentError> {
    if !config.headless_standby {
        depart_headless_standby(driver, standby)?;
        return Ok(false);
    }

    let active_targets = match launcher.probe_active_display_targets() {
        Ok(targets) => targets,
        Err(LauncherError::ConsoleSessionUnavailable) => return Ok(standby.is_some()),
        Err(err) => return Err(err.into()),
    };

    let own_target = standby.and_then(|s| s.target);
    let has_other_active_display = active_targets
        .iter()
        .any(|target| Some(*target) != own_target);

    if has_other_active_display {
        depart_headless_standby(driver, standby)?;
        return Ok(false);
    }

    // WHY: Verify the tracked standby display is still live in CCD so GPU TDR, `WUDFHost`
    // restarts, or manual OS disablement self-heal instead of stranding the host at 0 displays.
    let own_still_active = own_target.is_some_and(|t| active_targets.contains(&t));
    if standby.is_some() && own_still_active {
        return Ok(true);
    }
    *standby = None;

    let guid = monitor_guid_for_name(HEADLESS_STANDBY_CLIENT_ID);
    let (refresh_num, refresh_den) = millihz_to_rational(HEADLESS_STANDBY_REFRESH_MHZ);
    let mut req = AddDisplayRequest::new(
        guid,
        HEADLESS_STANDBY_WIDTH,
        HEADLESS_STANDBY_HEIGHT,
        refresh_num,
        refresh_den,
        HdrProfile::Sdr,
        OwnershipMode::HandleBound,
    );
    req.set_client_name(HEADLESS_STANDBY_CLIENT_ID)?;
    let resp = driver.add_display(&req)?;
    let target = (
        zerovdd_client::topology::AdapterLuid::new(resp.adapter_luid_low, resp.adapter_luid_high),
        resp.target_id,
    );
    // WHY: Explicitly activate standby via CCD Extend so Windows enables it at `(0, 0)` even if
    // the CCD database previously remembered the GUID as disabled or the lock screen is active,
    // without disabling any physical monitor that might wake up concurrently.
    let apply_payload = ApplyCcdPayload {
        adapter_luid_low: resp.adapter_luid_low,
        adapter_luid_high: resp.adapter_luid_high,
        target_id: resp.target_id,
        width: HEADLESS_STANDBY_WIDTH,
        height: HEADLESS_STANDBY_HEIGHT,
        refresh_num,
        refresh_den,
        hdr: false,
        topology_mode: SunshineTopologyMode::Extend,
        snapshot_path: None,
        extend_origin: None,
        make_virtual_primary: false,
        rotation: zerovdd_client::topology::DisplayRotation::Identity,
    };
    if let Err(err) = launcher.apply(&apply_payload) {
        eprintln!("zerovdd-svc headless standby CCD activation warning: {err}");
    }
    *standby = Some(StandbyDisplay {
        guid,
        target: Some(target),
    });
    Ok(true)
}

/// Departs the active headless-standby virtual display if one is currently attached.
pub fn depart_headless_standby(
    driver: &dyn ControlClient,
    standby: &mut Option<StandbyDisplay>,
) -> Result<bool, PersistentError> {
    let Some(active) = standby.take() else {
        return Ok(false);
    };
    match driver.remove_display(&RemoveDisplayRequest::by_guid(active.guid)) {
        Ok(()) => Ok(true),
        Err(ClientError::MonitorNotFound) => Ok(false),
        Err(err) => Err(err.into()),
    }
}
