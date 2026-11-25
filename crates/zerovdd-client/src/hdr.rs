//! Windows 11 24H2+ HDR and Auto Color Management (WCG) state management.

use core::mem::size_of;

use serde::{Deserialize, Serialize};

use crate::error::ClientError;
use crate::topology::AdapterLuid;

/// First Windows 11 24H2 OS build supporting split `SET_HDR_STATE` and `SET_WCG_STATE` APIs.
pub const WIN11_24H2_BUILD: u32 = 26_100;

/// Win32 `DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2` packet discriminator (`15`).
pub const DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2: i32 = 15;

/// Win32 `DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE` packet discriminator (`16`).
pub const DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE: i32 = 16;

/// Win32 `DISPLAYCONFIG_DEVICE_INFO_SET_WCG_STATE` packet discriminator (`17`).
pub const DISPLAYCONFIG_DEVICE_INFO_SET_WCG_STATE: i32 = 17;

/// Bit 0 of `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2::value`: `advancedColorSupported`.
pub const ADVANCED_COLOR_INFO_2_SUPPORTED_BIT: u32 = 1 << 0;

/// Bit 1 of `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2::value`: `advancedColorActive`.
pub const ADVANCED_COLOR_INFO_2_ACTIVE_BIT: u32 = 1 << 1;

/// Bit 3 of `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2::value`: `advancedColorLimitedByPolicy`.
pub const ADVANCED_COLOR_INFO_2_LIMITED_BY_POLICY_BIT: u32 = 1 << 3;

/// Bit 4 of `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2::value`: `highDynamicRangeSupported`.
pub const ADVANCED_COLOR_INFO_2_HDR_SUPPORTED_BIT: u32 = 1 << 4;

/// Bit 5 of `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2::value`: `highDynamicRangeUserEnabled`.
pub const ADVANCED_COLOR_INFO_2_HDR_USER_ENABLED_BIT: u32 = 1 << 5;

/// Bit 6 of `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2::value`: `wideColorSupported`.
pub const ADVANCED_COLOR_INFO_2_WCG_SUPPORTED_BIT: u32 = 1 << 6;

/// Bit 7 of `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2::value`: `wideColorUserEnabled`.
pub const ADVANCED_COLOR_INFO_2_WCG_USER_ENABLED_BIT: u32 = 1 << 7;

/// Bit 0 of legacy `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::value`: `advancedColorSupported`.
pub const ADVANCED_COLOR_INFO_V1_SUPPORTED_BIT: u32 = 1 << 0;

/// Bit 1 of legacy `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::value`: `advancedColorEnabled`.
pub const ADVANCED_COLOR_INFO_V1_ENABLED_BIT: u32 = 1 << 1;

/// Bit 2 of legacy `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::value`: `wideColorEnforced`.
pub const ADVANCED_COLOR_INFO_V1_WCG_ENFORCED_BIT: u32 = 1 << 2;

/// Bit 3 of legacy `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::value`: `advancedColorForceDisabled`.
pub const ADVANCED_COLOR_INFO_V1_FORCE_DISABLED_BIT: u32 = 1 << 3;

/// Raw `#[repr(C)]` Windows `LUID` layout matching `Win32::Foundation::LUID`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawDisplayConfigLuid {
    /// Low 32 bits of the LUID.
    pub low_part: u32,
    /// High 32 bits of the LUID.
    pub high_part: i32,
}

/// Raw `#[repr(C)]` layout matching `DISPLAYCONFIG_DEVICE_INFO_HEADER`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawDisplayConfigDeviceInfoHeader {
    /// `DISPLAYCONFIG_DEVICE_INFO_TYPE` discriminator.
    pub info_type: i32,
    /// Total struct size in bytes.
    pub size: u32,
    /// Target adapter LUID.
    pub adapter_id: RawDisplayConfigLuid,
    /// Target or source identifier.
    pub id: u32,
}

/// Exact `#[repr(C)]` FFI layout for Win11 24H2 `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DisplayConfigGetAdvancedColorInfo2 {
    /// Standard CCD device info header (`info_type = 15`, `size = 36`).
    pub header: RawDisplayConfigDeviceInfoHeader,
    /// Packed capability and user-toggle bitfield (`supported`, `active`, `hdr*`, `wcg*`).
    pub value: u32,
    /// Active `DISPLAYCONFIG_COLOR_ENCODING` numeric value.
    pub color_encoding: i32,
    /// Active bits per color channel (e.g. `8`, `10`, `12`).
    pub bits_per_color_channel: u32,
    /// Active `DISPLAYCONFIG_ADVANCED_COLOR_MODE` (`0 = SDR`, `1 = WCG`, `2 = HDR`).
    pub active_color_mode: i32,
}

/// Exact `#[repr(C)]` FFI layout for Win11 24H2 `DISPLAYCONFIG_SET_HDR_STATE`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DisplayConfigSetHdrState {
    /// Standard CCD device info header (`info_type = 16`, `size = 24`).
    pub header: RawDisplayConfigDeviceInfoHeader,
    /// Bit 0 (`1`) enables HDR; `0` disables HDR without disturbing ACM/WCG.
    pub value: u32,
}

/// Exact `#[repr(C)]` FFI layout for Win11 24H2 `DISPLAYCONFIG_SET_WCG_STATE`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DisplayConfigSetWcgState {
    /// Standard CCD device info header (`info_type = 17`, `size = 24`).
    pub header: RawDisplayConfigDeviceInfoHeader,
    /// Bit 0 (`1`) enables Auto Color Management (WCG); `0` disables ACM/WCG.
    pub value: u32,
}

const _: () = assert!(size_of::<RawDisplayConfigDeviceInfoHeader>() == 20);
const _: () = assert!(size_of::<DisplayConfigGetAdvancedColorInfo2>() == 36);
const _: () = assert!(size_of::<DisplayConfigSetHdrState>() == 24);
const _: () = assert!(size_of::<DisplayConfigSetWcgState>() == 24);

/// Windows OS version tuple queried from `ntdll!RtlGetVersion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsVersionInfo {
    /// OS major version (e.g. `10`).
    pub major: u32,
    /// OS minor version (e.g. `0`).
    pub minor: u32,
    /// OS build number (e.g. `26100` for Win11 24H2, `22631` for Win11 23H2).
    pub build: u32,
}

impl OsVersionInfo {
    /// Returns `true` when this OS build supports Win11 24H2 split HDR/WCG CCD APIs.
    pub const fn supports_split_hdr_wcg(self) -> bool {
        self.build >= WIN11_24H2_BUILD
    }
}

/// Active Windows advanced color pipeline mode reported by `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvancedColorMode {
    /// Standard Dynamic Range (`0`).
    #[default]
    Sdr = 0,
    /// Wide Color Gamut / Auto Color Management (`1`).
    Wcg = 1,
    /// High Dynamic Range (`2`).
    Hdr = 2,
}

impl AdvancedColorMode {
    /// Converts a raw `DISPLAYCONFIG_ADVANCED_COLOR_MODE` integer into [`AdvancedColorMode`].
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::Sdr),
            1 => Some(Self::Wcg),
            2 => Some(Self::Hdr),
            _ => None,
        }
    }
}

/// Independent HDR and Auto Color Management (WCG) state for a display target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisplayColorState {
    /// Whether the display supports HDR10 output.
    pub hdr_supported: bool,
    /// Whether the user has HDR enabled on this display.
    pub hdr_enabled: bool,
    /// Whether the display supports Auto Color Management / WCG (Win11 24H2+).
    #[serde(default)]
    pub wcg_supported: bool,
    /// Whether Auto Color Management (WCG) is enabled independently of HDR (Win11 24H2+).
    #[serde(default)]
    pub wcg_acm_enabled: bool,
    /// Whether advanced color is restricted by group/driver policy.
    #[serde(default)]
    pub limited_by_policy: bool,
    /// Bits per color channel currently active on the target.
    #[serde(default)]
    pub bits_per_color_channel: u32,
    /// Active pipeline color mode (`Sdr`, `Wcg`, or `Hdr`).
    #[serde(default)]
    pub active_color_mode: AdvancedColorMode,
    /// Raw `DISPLAYCONFIG_COLOR_ENCODING` numeric value.
    #[serde(default)]
    pub color_encoding: i32,
}

/// Desired HDR and Auto Color Management (WCG) configuration for a display target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HdrTargetSetting {
    /// Adapter LUID owning the target.
    pub adapter_luid: AdapterLuid,
    /// OS VidPN target identifier.
    pub target_id: u32,
    /// Desired HDR enabled state.
    pub hdr_enabled: bool,
    /// Optional desired Auto Color Management (WCG) enabled state on Win11 24H2+.
    #[serde(default)]
    pub wcg_acm_enabled: Option<bool>,
}

/// Ordered CCD color state mutation action planned for a specific OS build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorApiAction {
    /// Win11 24H2+ `DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE` (`type = 16`).
    SetHdrState {
        /// Desired HDR enabled state.
        enable: bool,
    },
    /// Win11 24H2+ `DISPLAYCONFIG_DEVICE_INFO_SET_WCG_STATE` (`type = 17`).
    SetWcgState {
        /// Desired ACM/WCG enabled state.
        enable: bool,
    },
    /// Legacy pre-24H2 `DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE` (`type = 10`).
    SetAdvancedColorState {
        /// Desired advanced color (HDR) enabled state.
        enable: bool,
    },
}

/// Decodes a Win11 24H2+ `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` response into [`DisplayColorState`].
pub fn decode_advanced_color_info_2(
    value: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode_raw: i32,
) -> DisplayColorState {
    let limited_by_policy = (value & ADVANCED_COLOR_INFO_2_LIMITED_BY_POLICY_BIT) != 0;
    let hdr_supported =
        (value & ADVANCED_COLOR_INFO_2_HDR_SUPPORTED_BIT) != 0 && !limited_by_policy;
    let hdr_enabled = (value & ADVANCED_COLOR_INFO_2_HDR_USER_ENABLED_BIT) != 0;
    let wcg_supported = (value & ADVANCED_COLOR_INFO_2_WCG_SUPPORTED_BIT) != 0;
    let wcg_acm_enabled = (value & ADVANCED_COLOR_INFO_2_WCG_USER_ENABLED_BIT) != 0;
    let active_color_mode =
        AdvancedColorMode::from_raw(active_color_mode_raw).unwrap_or(if hdr_enabled {
            AdvancedColorMode::Hdr
        } else if wcg_acm_enabled {
            AdvancedColorMode::Wcg
        } else {
            AdvancedColorMode::Sdr
        });

    DisplayColorState {
        hdr_supported,
        hdr_enabled,
        wcg_supported,
        wcg_acm_enabled,
        limited_by_policy,
        bits_per_color_channel,
        active_color_mode,
        color_encoding,
    }
}

/// Decodes a pre-24H2 `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO` response into [`DisplayColorState`].
pub fn decode_advanced_color_info_v1(
    value: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
) -> DisplayColorState {
    let force_disabled = (value & ADVANCED_COLOR_INFO_V1_FORCE_DISABLED_BIT) != 0;
    let hdr_supported = (value & ADVANCED_COLOR_INFO_V1_SUPPORTED_BIT) != 0 && !force_disabled;
    let hdr_enabled = (value & ADVANCED_COLOR_INFO_V1_ENABLED_BIT) != 0;
    let active_color_mode = if hdr_enabled {
        AdvancedColorMode::Hdr
    } else {
        AdvancedColorMode::Sdr
    };

    DisplayColorState {
        hdr_supported,
        hdr_enabled,
        wcg_supported: false,
        wcg_acm_enabled: false,
        limited_by_policy: force_disabled,
        bits_per_color_channel,
        active_color_mode,
        color_encoding,
    }
}

/// Computes the exact CCD device-info calls required to apply `setting` on `os_build`.
pub fn plan_color_actions(
    target_id: u32,
    current: DisplayColorState,
    setting: &HdrTargetSetting,
    os_build: u32,
) -> Result<Vec<ColorApiAction>, ClientError> {
    if setting.hdr_enabled && !current.hdr_supported {
        let reason = if current.limited_by_policy {
            "HDR is blocked by display policy".to_string()
        } else {
            "target display does not advertise HDR support".to_string()
        };
        return Err(ClientError::HdrUnsupported { target_id, reason });
    }

    let mut actions = Vec::new();
    if os_build >= WIN11_24H2_BUILD {
        // WHY: On Windows 11 24H2+, legacy SET_ADVANCED_COLOR_STATE fails with ERROR_NOT_SUPPORTED
        // or conflates Auto Color Management with HDR; SET_HDR_STATE and SET_WCG_STATE are independent.
        if current.hdr_enabled != setting.hdr_enabled {
            actions.push(ColorApiAction::SetHdrState {
                enable: setting.hdr_enabled,
            });
        }
        if let Some(want_wcg) = setting.wcg_acm_enabled {
            if current.wcg_supported && current.wcg_acm_enabled != want_wcg {
                actions.push(ColorApiAction::SetWcgState { enable: want_wcg });
            }
        }
    } else if current.hdr_enabled != setting.hdr_enabled {
        actions.push(ColorApiAction::SetAdvancedColorState {
            enable: setting.hdr_enabled,
        });
    }

    Ok(actions)
}

#[cfg(windows)]
mod win32 {
    use core::mem::{align_of, size_of};

    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo,
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_DEVICE_INFO_TYPE,
        DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE,
    };
    use windows::Win32::Foundation::{ERROR_NOT_SUPPORTED, ERROR_SUCCESS, LUID, NTSTATUS};
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    use super::{
        decode_advanced_color_info_2, decode_advanced_color_info_v1, plan_color_actions,
        ColorApiAction, DisplayColorState, DisplayConfigGetAdvancedColorInfo2,
        DisplayConfigSetHdrState, DisplayConfigSetWcgState, HdrTargetSetting, OsVersionInfo,
        RawDisplayConfigDeviceInfoHeader, RawDisplayConfigLuid,
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2,
        DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE, DISPLAYCONFIG_DEVICE_INFO_SET_WCG_STATE,
        WIN11_24H2_BUILD,
    };
    use crate::error::ClientError;
    use crate::topology::AdapterLuid;

    const _: () = assert!(
        size_of::<RawDisplayConfigDeviceInfoHeader>()
            == size_of::<DISPLAYCONFIG_DEVICE_INFO_HEADER>()
    );
    const _: () = assert!(
        align_of::<RawDisplayConfigDeviceInfoHeader>()
            == align_of::<DISPLAYCONFIG_DEVICE_INFO_HEADER>()
    );

    // SAFETY: `ntdll!RtlGetVersion` matches the documented Win32/WDK `OSVERSIONINFOW` ABI.
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(lpversioninformation: *mut OSVERSIONINFOW) -> NTSTATUS;
    }

    fn make_raw_header(
        info_type: i32,
        size: usize,
        adapter_luid: AdapterLuid,
        target_id: u32,
    ) -> RawDisplayConfigDeviceInfoHeader {
        RawDisplayConfigDeviceInfoHeader {
            info_type,
            size: size as u32,
            adapter_id: RawDisplayConfigLuid {
                low_part: adapter_luid.low,
                high_part: adapter_luid.high,
            },
            id: target_id,
        }
    }

    /// Queries the true Windows OS version via `ntdll!RtlGetVersion` (unaffected by app manifests).
    pub fn query_os_version() -> Result<OsVersionInfo, ClientError> {
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is stack-allocated with `dwOSVersionInfoSize` initialized to `size_of::<OSVERSIONINFOW>()`.
        let status = unsafe { RtlGetVersion(&mut info) };
        if status.is_err() {
            return Err(ClientError::Win32 {
                operation: "RtlGetVersion",
                code: status.0 as u32,
            });
        }
        Ok(OsVersionInfo {
            major: info.dwMajorVersion,
            minor: info.dwMinorVersion,
            build: info.dwBuildNumber,
        })
    }

    /// Queries a display target's HDR and Auto Color Management state for `os_build`.
    pub fn query_display_color_state(
        adapter_luid: AdapterLuid,
        target_id: u32,
        os_build: u32,
    ) -> Result<DisplayColorState, ClientError> {
        if os_build >= WIN11_24H2_BUILD {
            let mut pkt = DisplayConfigGetAdvancedColorInfo2 {
                header: make_raw_header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2,
                    size_of::<DisplayConfigGetAdvancedColorInfo2>(),
                    adapter_luid,
                    target_id,
                ),
                ..Default::default()
            };
            // SAFETY: `DisplayConfigGetAdvancedColorInfo2` is `#[repr(C)]` starting with a
            // layout-identical `DISPLAYCONFIG_DEVICE_INFO_HEADER` whose `size` equals `36`.
            let status = unsafe {
                DisplayConfigGetDeviceInfo(
                    (&raw mut pkt).cast::<DISPLAYCONFIG_DEVICE_INFO_HEADER>(),
                )
            };
            if status as u32 == ERROR_SUCCESS.0 {
                return Ok(decode_advanced_color_info_2(
                    pkt.value,
                    pkt.color_encoding,
                    pkt.bits_per_color_channel,
                    pkt.active_color_mode,
                ));
            }
            if status as u32 != ERROR_NOT_SUPPORTED.0 {
                return Err(ClientError::Ccd {
                    operation: "DisplayConfigGetDeviceInfo(GET_ADVANCED_COLOR_INFO_2)",
                    status,
                });
            }
        }

        let mut pkt = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                adapterId: LUID {
                    LowPart: adapter_luid.low,
                    HighPart: adapter_luid.high,
                },
                id: target_id,
            },
            ..Default::default()
        };
        // SAFETY: `pkt` is stack-allocated and initialized with exact `header.size`.
        let status = unsafe { DisplayConfigGetDeviceInfo(&mut pkt.header) };
        if status as u32 != ERROR_SUCCESS.0 {
            return Err(ClientError::Ccd {
                operation: "DisplayConfigGetDeviceInfo(GET_ADVANCED_COLOR_INFO)",
                status,
            });
        }
        // SAFETY: `pkt.Anonymous.value` is the active `u32` representation of the C union bitfield.
        let value = unsafe { pkt.Anonymous.value };
        Ok(decode_advanced_color_info_v1(
            value,
            pkt.colorEncoding.0,
            pkt.bitsPerColorChannel,
        ))
    }

    fn apply_single_color_action(
        action: ColorApiAction,
        setting: HdrTargetSetting,
    ) -> Result<(), ClientError> {
        match action {
            ColorApiAction::SetHdrState { enable } => {
                let pkt = DisplayConfigSetHdrState {
                    header: make_raw_header(
                        DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE,
                        size_of::<DisplayConfigSetHdrState>(),
                        setting.adapter_luid,
                        setting.target_id,
                    ),
                    value: u32::from(enable),
                };
                // SAFETY: `DisplayConfigSetHdrState` is `#[repr(C)]` starting with a
                // layout-identical `DISPLAYCONFIG_DEVICE_INFO_HEADER` whose `size` equals `24`.
                let status = unsafe {
                    DisplayConfigSetDeviceInfo(
                        (&raw const pkt).cast::<DISPLAYCONFIG_DEVICE_INFO_HEADER>(),
                    )
                };
                if status as u32 != ERROR_SUCCESS.0 {
                    if status as u32 == ERROR_NOT_SUPPORTED.0 {
                        return Err(ClientError::HdrUnsupported {
                            target_id: setting.target_id,
                            reason: "DisplayConfigSetDeviceInfo(SET_HDR_STATE) returned ERROR_NOT_SUPPORTED".to_string(),
                        });
                    }
                    return Err(ClientError::Ccd {
                        operation: "DisplayConfigSetDeviceInfo(SET_HDR_STATE)",
                        status,
                    });
                }
            }
            ColorApiAction::SetWcgState { enable } => {
                let pkt = DisplayConfigSetWcgState {
                    header: make_raw_header(
                        DISPLAYCONFIG_DEVICE_INFO_SET_WCG_STATE,
                        size_of::<DisplayConfigSetWcgState>(),
                        setting.adapter_luid,
                        setting.target_id,
                    ),
                    value: u32::from(enable),
                };
                // SAFETY: `DisplayConfigSetWcgState` is `#[repr(C)]` starting with a
                // layout-identical `DISPLAYCONFIG_DEVICE_INFO_HEADER` whose `size` equals `24`.
                let status = unsafe {
                    DisplayConfigSetDeviceInfo(
                        (&raw const pkt).cast::<DISPLAYCONFIG_DEVICE_INFO_HEADER>(),
                    )
                };
                if status as u32 != ERROR_SUCCESS.0 {
                    return Err(ClientError::Ccd {
                        operation: "DisplayConfigSetDeviceInfo(SET_WCG_STATE)",
                        status,
                    });
                }
            }
            ColorApiAction::SetAdvancedColorState { enable } => {
                let mut pkt = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
                    header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                        r#type: DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                        size: size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as u32,
                        adapterId: LUID {
                            LowPart: setting.adapter_luid.low,
                            HighPart: setting.adapter_luid.high,
                        },
                        id: setting.target_id,
                    },
                    ..Default::default()
                };
                pkt.Anonymous.value = u32::from(enable);
                // SAFETY: `pkt` is stack-allocated with exact `header.size` and `value` populated.
                let status = unsafe { DisplayConfigSetDeviceInfo(&pkt.header) };
                if status as u32 != ERROR_SUCCESS.0 {
                    if status as u32 == ERROR_NOT_SUPPORTED.0 {
                        return Err(ClientError::HdrUnsupported {
                            target_id: setting.target_id,
                            reason: "DisplayConfigSetDeviceInfo(SET_ADVANCED_COLOR_STATE) returned ERROR_NOT_SUPPORTED".to_string(),
                        });
                    }
                    return Err(ClientError::Ccd {
                        operation: "DisplayConfigSetDeviceInfo(SET_ADVANCED_COLOR_STATE)",
                        status,
                    });
                }
            }
        }
        Ok(())
    }

    /// Queries the current state and applies `setting` using OS-appropriate CCD color APIs.
    pub fn apply_hdr_setting(setting: &HdrTargetSetting, os_build: u32) -> Result<(), ClientError> {
        let current = query_display_color_state(setting.adapter_luid, setting.target_id, os_build)?;
        let actions = plan_color_actions(setting.target_id, current, setting, os_build)?;
        for action in actions {
            crate::ccd::run_on_input_desktop_if_access_denied(|| {
                apply_single_color_action(action, *setting)
            })?;
        }
        Ok(())
    }

    // WHY: Keep unused-enum-variant checks happy if DISPLAYCONFIG_DEVICE_INFO_TYPE is referenced cleanly.
    const _: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
}

#[cfg(windows)]
pub use win32::{apply_hdr_setting, query_display_color_state, query_os_version};
