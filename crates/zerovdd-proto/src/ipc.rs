//! Versioned length-prefixed JSON IPC protocol between `zerovdd-cli` and `zerovdd-svc`.

use std::io::{Read, Write};
use std::string::String;
use std::vec::Vec;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{
    BitDepthPreference, ColorSpacePreference, HdrMasteringMetadata, HdrProfile, MAX_CUSTOM_EDID_LEN,
};
use crate::validation::{
    validate_color_and_bit_depth, validate_dimensions, validate_hdr_metadata,
    validate_refresh_rate, validate_target_ppi, ProtoError, MAX_LEASE_TTL_SECS,
};

/// Canonical Windows named pipe path shared by `zerovdd-svc` and `zerovdd-cli`.
pub const IPC_PIPE_NAME: &str = r"\\.\pipe\zerovdd-svc";

/// Current service named-pipe IPC protocol version.
pub const IPC_PROTOCOL_VERSION: u16 = 1;

/// Maximum permitted IPC frame payload size in bytes (1 MiB).
pub const MAX_IPC_FRAME_BYTES: usize = 1024 * 1024;

/// Maximum permitted length for an IPC `client_id` string.
pub const MAX_IPC_CLIENT_ID_LEN: usize = 128;

/// Errors returned by IPC frame encoding, decoding, and payload validation.
#[derive(Debug, Error)]
pub enum IpcError {
    /// Frame buffer is shorter than the 4-byte length prefix.
    #[error("IPC frame truncated: expected at least 4 length-prefix bytes, got {actual}")]
    TruncatedHeader {
        /// Actual bytes available.
        actual: usize,
    },

    /// Frame declared a zero-byte payload.
    #[error("IPC frame payload length must be greater than zero")]
    EmptyFrame,

    /// Frame declared a payload exceeding [`MAX_IPC_FRAME_BYTES`].
    #[error("IPC frame size {size} exceeds maximum {MAX_IPC_FRAME_BYTES} bytes")]
    FrameTooLarge {
        /// Declared payload size in bytes.
        size: usize,
    },

    /// Slice length does not match the 4-byte header plus declared payload length.
    #[error(
        "IPC frame length mismatch: header declared {declared} bytes, got {actual} payload bytes"
    )]
    FrameLengthMismatch {
        /// Declared payload size from the frame prefix.
        declared: usize,
        /// Actual payload bytes present after the prefix.
        actual: usize,
    },

    /// IPC envelope version does not match [`IPC_PROTOCOL_VERSION`].
    #[error("unsupported IPC protocol version {actual}; expected {expected}")]
    UnsupportedVersion {
        /// Expected IPC protocol version.
        expected: u16,
        /// Received IPC protocol version.
        actual: u16,
    },

    /// IPC request failed domain validation.
    #[error("invalid IPC payload: {0}")]
    InvalidPayload(#[from] ProtoError),

    /// Client identifier is empty, too long, or contains control characters.
    #[error("invalid IPC client_id")]
    InvalidClientId,

    /// JSON serialization or deserialization failed.
    #[error("IPC JSON codec error: {0}")]
    Json(#[from] serde_json::Error),

    /// Underlying stream I/O failed while reading or writing a frame.
    #[error("IPC I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Clockwise virtual display orientation serialized over service JSON IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayOrientation {
    /// Standard unrotated landscape orientation (`0` degrees).
    #[default]
    Identity,
    /// Rotated `90` degrees clockwise.
    Rotate90,
    /// Rotated `180` degrees.
    Rotate180,
    /// Rotated `270` degrees clockwise.
    Rotate270,
}

impl DisplayOrientation {
    /// Returns the clockwise angle in degrees (`0`, `90`, `180`, `270`).
    pub const fn as_degrees(self) -> u16 {
        match self {
            Self::Identity => 0,
            Self::Rotate90 => 90,
            Self::Rotate180 => 180,
            Self::Rotate270 => 270,
        }
    }

    /// Converts clockwise angle in degrees (`0`, `90`, `180`, `270`) to [`DisplayOrientation`].
    pub const fn from_degrees(deg: u16) -> Option<Self> {
        match deg {
            0 => Some(Self::Identity),
            90 => Some(Self::Rotate90),
            180 => Some(Self::Rotate180),
            270 => Some(Self::Rotate270),
            _ => None,
        }
    }
}

/// Metadata summary of an active virtual display returned by [`IpcResponse::DisplayList`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveDisplayInfo {
    /// Hyphenated monitor container UUID.
    pub monitor_guid: String,
    /// Configured client name label.
    pub client_name: String,
    /// Windows adapter LUID low part.
    pub adapter_luid_low: u32,
    /// Windows adapter LUID high part.
    pub adapter_luid_high: i32,
    /// OS VidPN target identifier.
    pub target_id: u32,
    /// Internal connector slot index.
    pub connector_index: u32,
    /// Active mode width in pixels.
    pub width: u32,
    /// Active mode height in pixels.
    pub height: u32,
    /// Active rational refresh numerator.
    pub refresh_num: u32,
    /// Active rational refresh denominator.
    pub refresh_den: u32,
    /// Whether HDR10 profile is active.
    pub hdr_active: bool,
    /// Ownership mode byte (`0` = handle-bound, `1` = leased).
    pub ownership_mode: u8,
    /// Remaining lease duration in seconds (`0` for handle-bound).
    pub lease_remaining_secs: u32,
}

/// Summary of an enumerated DXGI GPU adapter returned by [`IpcResponse::GpuList`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuAdapterSummary {
    /// Adapter description reported by DXGI.
    pub friendly_name: String,
    /// Low 32 bits of the adapter LUID.
    pub adapter_luid_low: u32,
    /// High 32 bits of the adapter LUID.
    pub adapter_luid_high: i32,
    /// PCI bus number, if known.
    pub pci_bus: Option<u32>,
    /// PCI device number, if known.
    pub pci_device: Option<u32>,
    /// PCI function number, if known.
    pub pci_function: Option<u32>,
    /// Dedicated VRAM in bytes.
    pub dedicated_vram_bytes: u64,
    /// `true` for software fallback adapters (e.g. Microsoft Basic Render Driver).
    pub is_software: bool,
    /// `true` if selected by automatic best-GPU ranking or currently configured.
    pub is_best: bool,
}

/// Request sent to switch the active render GPU adapter via `zerovdd-svc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchGpuRequest {
    /// Selector string (`auto`, `luid:<low>:<high>`, `pci:<bus>[:<dev>.<fn>]`, or friendly name).
    pub selector: String,
}

impl SwitchGpuRequest {
    /// Validates that the GPU selector string is non-empty, bounded, and control-char free.
    pub fn validate(&self) -> Result<(), IpcError> {
        validate_ipc_client_id(&self.selector)
    }
}

/// Color gamut preference serialized over the service JSON IPC pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpace {
    /// Automatically choose BT.709 for SDR or BT.2020 for HDR10.
    #[default]
    Auto,
    /// ITU-R BT.709 / sRGB primaries (SDR).
    Bt709,
    /// ITU-R BT.2020 wide color gamut primaries (HDR10).
    Bt2020,
    /// Display P3 / DCI-P3 primaries (HDR10).
    DciP3,
}

impl From<ColorSpace> for ColorSpacePreference {
    fn from(value: ColorSpace) -> Self {
        match value {
            ColorSpace::Auto => Self::Auto,
            ColorSpace::Bt709 => Self::Bt709,
            ColorSpace::Bt2020 => Self::Bt2020,
            ColorSpace::DciP3 => Self::DciP3,
        }
    }
}

/// Color bit-depth preference per component serialized over the service JSON IPC pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BitDepth {
    /// Automatic defaults (`8|10` bpc for SDR, `10|12` bpc for HDR10).
    #[default]
    Auto,
    /// 8 bits per channel (`8` bpc, SDR only).
    Bpc8,
    /// 10 bits per channel (`10` bpc).
    Bpc10,
    /// 12 bits per channel (`12` bpc).
    Bpc12,
}

impl From<BitDepth> for BitDepthPreference {
    fn from(value: BitDepth) -> Self {
        match value {
            BitDepth::Auto => Self::Auto,
            BitDepth::Bpc8 => Self::Bpc8,
            BitDepth::Bpc10 => Self::Bpc10,
            BitDepth::Bpc12 => Self::Bpc12,
        }
    }
}

/// Display topology arrangement requested when starting a streaming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyMode {
    /// Disable physical monitors and set the virtual display as sole primary at `(0, 0)`.
    Isolate,
    /// Keep physical monitors enabled and extend the desktop with the virtual display.
    Extend,
}

/// Versioned envelope wrapping every IPC request or response message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcEnvelope<T> {
    /// Wire schema version (must equal [`IPC_PROTOCOL_VERSION`]).
    pub version: u16,
    /// Caller-assigned correlation identifier echoed by responses.
    pub request_id: u64,
    /// Inner typed request or response payload.
    pub payload: T,
}

impl<T> IpcEnvelope<T> {
    /// Wraps `payload` in an envelope stamped with [`IPC_PROTOCOL_VERSION`].
    pub const fn new(request_id: u64, payload: T) -> Self {
        Self {
            version: IPC_PROTOCOL_VERSION,
            request_id,
            payload,
        }
    }

    /// Validates that the envelope version matches [`IPC_PROTOCOL_VERSION`].
    pub fn validate_version(&self) -> Result<(), IpcError> {
        if self.version != IPC_PROTOCOL_VERSION {
            return Err(IpcError::UnsupportedVersion {
                expected: IPC_PROTOCOL_VERSION,
                actual: self.version,
            });
        }
        Ok(())
    }
}

/// Request sent by `zerovdd-cli` to start a Sunshine-managed virtual display session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSessionRequest {
    /// Stable client identifier (e.g. Moonlight client UUID or sanitized host name).
    pub client_id: String,
    /// Requested virtual display width in pixels.
    pub width: u32,
    /// Requested virtual display height in pixels.
    pub height: u32,
    /// Requested refresh rate in millihertz (e.g. `60_000` or `119_880`).
    pub refresh_mhz: u32,
    /// Whether HDR10 should be advertised and enabled on the virtual display.
    pub hdr: bool,
    /// Process ID of the owning Sunshine host process for crash supervision.
    pub owner_pid: u32,
    /// Windows process creation timestamp (100ns FILETIME ticks) to prevent PID reuse races.
    pub owner_creation_time: u64,
    /// Requested desktop topology mode (`isolate` vs `extend`).
    pub topology_mode: TopologyMode,
}

impl StartSessionRequest {
    /// Validates client identifier, display geometry, and millihertz refresh rate.
    pub fn validate(&self) -> Result<(), IpcError> {
        validate_ipc_client_id(&self.client_id)?;
        validate_dimensions(self.width, self.height)?;
        validate_refresh_rate(self.refresh_mhz, 1000)?;
        Ok(())
    }
}

/// Request sent to stop an active streaming session and restore display topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopSessionRequest {
    /// Optional client identifier filter; `None` stops the active session.
    pub client_id: Option<String>,
    /// Optional owning process ID filter.
    pub owner_pid: Option<u32>,
    /// Force teardown even if `owner_pid` does not match the recorded session owner.
    pub force: bool,
}

impl StopSessionRequest {
    /// Validates optional client identifier constraints.
    pub fn validate(&self) -> Result<(), IpcError> {
        if let Some(ref client_id) = self.client_id {
            validate_ipc_client_id(client_id)?;
        }
        Ok(())
    }
}

/// Request sent to add a standalone or leased virtual display via the service.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisplayAddRequest {
    /// Client/label identifier used to derive the monitor GUID and EDID identity.
    pub client_id: String,
    /// Requested width in pixels.
    pub width: u32,
    /// Requested height in pixels.
    pub height: u32,
    /// Requested refresh rate in millihertz.
    pub refresh_mhz: u32,
    /// Whether HDR10 profile is requested.
    pub hdr: bool,
    /// Optional lease TTL in seconds (`None` keeps service-managed ownership).
    pub lease_ttl_secs: Option<u32>,
    /// Optional desktop topology mode applied by the service inside the active console session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology_mode: Option<TopologyMode>,
    /// Optional target pixel density (`48..=600` PPI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ppi: Option<u16>,
    /// Optional explicit color space preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_space: Option<ColorSpace>,
    /// Optional explicit color bit-depth cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<BitDepth>,
    /// Optional peak HDR10 mastering luminance override in nits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_luminance_nits: Option<u16>,
    /// Optional max frame-average luminance override in nits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fall_nits: Option<u16>,
    /// Optional minimum mastering luminance override in `0.0001`-nit units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_luminance_ten_thousandths: Option<u32>,
    /// Optional raw custom EDID blocks (`128..=512` bytes, multiple of 128).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_edid: Option<Vec<u8>>,
    /// Optional explicit hyphenated monitor UUID string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guid: Option<String>,
    /// Optional render GPU selector (`auto`, `luid:...`, `pci:...`, or friendly name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
    /// Optional clockwise display orientation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<DisplayOrientation>,
    /// Optional desktop origin `(x, y)` in pixels when extending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
    /// Optional flag requesting virtual display placement at `(0, 0)` primary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub make_primary: Option<bool>,
}

fn is_valid_uuid_text(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() == 32 {
        return bytes.iter().all(|b| b.is_ascii_hexdigit());
    }
    if bytes.len() == 36 {
        return bytes.iter().enumerate().all(|(i, &b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        });
    }
    false
}

impl DisplayAddRequest {
    /// Validates client identifier, display geometry, millihertz refresh rate, lease TTL, EDID size, and layout/selector fields.
    pub fn validate(&self) -> Result<(), IpcError> {
        validate_ipc_client_id(&self.client_id)?;
        if let Some(ref guid) = self.guid {
            if !is_valid_uuid_text(guid.trim()) {
                return Err(IpcError::InvalidPayload(ProtoError::InvalidSelector));
            }
        }
        if let Some(ref gpu) = self.gpu {
            validate_ipc_client_id(gpu)?;
        }
        validate_dimensions(self.width, self.height)?;
        validate_refresh_rate(self.refresh_mhz, 1000)?;
        if let Some(ttl) = self.lease_ttl_secs {
            if ttl == 0 || ttl > MAX_LEASE_TTL_SECS {
                return Err(IpcError::InvalidPayload(ProtoError::InvalidLeaseTtl(ttl)));
            }
        }
        if let Some(ref edid) = self.custom_edid {
            if edid.len() > MAX_CUSTOM_EDID_LEN {
                return Err(IpcError::InvalidPayload(
                    ProtoError::FieldLengthOutOfBounds {
                        field: "custom_edid",
                        len: edid.len(),
                        max: MAX_CUSTOM_EDID_LEN,
                    },
                ));
            }
            if edid.len() < 128 || !edid.len().is_multiple_of(128) {
                return Err(IpcError::InvalidPayload(
                    ProtoError::InvalidCustomEdidLength(edid.len()),
                ));
            }
        }
        if let Some(ppi) = self.target_ppi {
            if ppi == 0 {
                return Err(IpcError::InvalidPayload(ProtoError::InvalidTargetPpi(ppi)));
            }
            validate_target_ppi(ppi)?;
        }
        let hdr_profile = if self.hdr {
            HdrProfile::Hdr10
        } else {
            HdrProfile::Sdr
        };
        validate_color_and_bit_depth(
            hdr_profile,
            self.color_space.unwrap_or_default().into(),
            self.bit_depth.unwrap_or_default().into(),
        )?;
        validate_hdr_metadata(HdrMasteringMetadata {
            max_luminance_nits: self.max_luminance_nits.unwrap_or(0),
            max_fall_nits: self.max_fall_nits.unwrap_or(0),
            min_luminance_ten_thousandths: self.min_luminance_ten_thousandths.unwrap_or(0),
        })?;
        Ok(())
    }
}

/// Request sent to remove a previously added virtual display via the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayRemoveRequest {
    /// Client identifier of the display to remove.
    pub client_id: Option<String>,
    /// VidPN target identifier of the display to remove.
    pub target_id: Option<u32>,
}

impl DisplayRemoveRequest {
    /// Validates that a valid client identifier or non-zero target ID selector is present.
    pub fn validate(&self) -> Result<(), IpcError> {
        if self.client_id.is_none() && self.target_id.is_none() {
            return Err(IpcError::InvalidPayload(ProtoError::InvalidSelector));
        }
        if let Some(ref client_id) = self.client_id {
            validate_ipc_client_id(client_id)?;
        }
        if let Some(target_id) = self.target_id {
            if target_id == 0 {
                return Err(IpcError::InvalidPayload(ProtoError::InvalidSelector));
            }
        }
        Ok(())
    }
}

/// Request sent to update the active resolution, refresh rate, or HDR profile on an existing virtual display.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisplaySetModeRequest {
    /// Optional client identifier or GUID string; `None` auto-selects when one display is active.
    pub client_id: Option<String>,
    /// Optional VidPN target ID; `None` auto-selects when `client_id` is also `None`.
    pub target_id: Option<u32>,
    /// Requested preferred width in pixels.
    pub width: u32,
    /// Requested preferred height in lines.
    pub height: u32,
    /// Requested preferred refresh rate in millihertz.
    pub refresh_mhz: u32,
    /// Optional HDR10 override (`None` preserves the active display's current HDR setting).
    pub hdr: Option<bool>,
    /// Optional explicit color space preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_space: Option<ColorSpace>,
    /// Optional explicit color bit-depth cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<BitDepth>,
    /// Optional peak HDR10 mastering luminance override in nits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_luminance_nits: Option<u16>,
    /// Optional max frame-average luminance override in nits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fall_nits: Option<u16>,
    /// Optional minimum mastering luminance override in `0.0001`-nit units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_luminance_ten_thousandths: Option<u32>,
}

impl DisplaySetModeRequest {
    /// Validates optional selector bounds, geometry, millihertz refresh rate, and color settings.
    pub fn validate(&self) -> Result<(), IpcError> {
        if self.client_id.is_some() && self.target_id.is_some() {
            return Err(IpcError::InvalidPayload(ProtoError::InvalidSelector));
        }
        if let Some(ref client_id) = self.client_id {
            validate_ipc_client_id(client_id)?;
        }
        if let Some(target_id) = self.target_id {
            if target_id == 0 {
                return Err(IpcError::InvalidPayload(ProtoError::InvalidSelector));
            }
        }
        validate_dimensions(self.width, self.height)?;
        validate_refresh_rate(self.refresh_mhz, 1000)?;
        if let Some(hdr) = self.hdr {
            let hdr_profile = if hdr {
                HdrProfile::Hdr10
            } else {
                HdrProfile::Sdr
            };
            validate_color_and_bit_depth(
                hdr_profile,
                self.color_space.unwrap_or_default().into(),
                self.bit_depth.unwrap_or_default().into(),
            )?;
        }
        validate_hdr_metadata(HdrMasteringMetadata {
            max_luminance_nits: self.max_luminance_nits.unwrap_or(0),
            max_fall_nits: self.max_fall_nits.unwrap_or(0),
            min_luminance_ten_thousandths: self.min_luminance_ten_thousandths.unwrap_or(0),
        })?;
        Ok(())
    }
}

/// Snapshot of the active streaming session tracked by `zerovdd-svc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    /// Whether a streaming virtual display session is currently active.
    pub active: bool,
    /// Active client identifier, if any.
    pub client_id: Option<String>,
    /// Resolved Windows GDI display name (for example `\\.\DISPLAY2`).
    pub display_name: Option<String>,
    /// Resolved VidPN target identifier.
    pub target_id: Option<u32>,
    /// Active width in pixels (`0` when inactive).
    pub width: u32,
    /// Active height in pixels (`0` when inactive).
    pub height: u32,
    /// Active refresh rate in millihertz (`0` when inactive).
    pub refresh_mhz: u32,
    /// Whether HDR is currently enabled on the virtual display.
    pub hdr_active: bool,
    /// Owning Sunshine PID being monitored by the service.
    pub owner_pid: Option<u32>,
    /// Active topology mode (`isolate` or `extend`).
    pub topology_mode: Option<TopologyMode>,
}

/// Service health and driver connectivity status reported by `zerovdd-svc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceHealth {
    /// Service crate semantic version string.
    pub service_version: String,
    /// Whether the service currently holds a healthy Control Device handle.
    pub driver_connected: bool,
    /// Driver protocol `(major, minor, patch)` version, if connected.
    pub protocol_version: Option<(u16, u16, u16)>,
    /// Number of active virtual displays reported by the driver.
    pub active_displays: u32,
    /// Service process uptime in seconds.
    pub uptime_secs: u64,
    /// Whether a durable topology backup file currently exists on disk.
    pub backup_present: bool,
}

/// Structured application-level error returned inside [`IpcResponse::Error`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcErrorPayload {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable diagnostic message.
    pub message: String,
}

/// Tagged IPC request variants accepted by `zerovdd-svc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcRequest {
    /// Start a streaming display session and configure topology.
    StartSession(StartSessionRequest),
    /// Stop the active streaming session and restore physical topology.
    StopSession(StopSessionRequest),
    /// Query current session status.
    GetSessionStatus,
    /// Add a virtual display and optionally apply desktop topology in the active console session.
    DisplayAdd(DisplayAddRequest),
    /// Remove a virtual display.
    DisplayRemove(DisplayRemoveRequest),
    /// Update mode and optional HDR state on an active virtual display in place.
    DisplaySetMode(DisplaySetModeRequest),
    /// List active virtual displays maintained by the driver.
    DisplayList,
    /// Enumerate DXGI render GPU adapters and current configuration.
    ListGpus,
    /// Switch the render GPU adapter used by the driver.
    SwitchGpu(SwitchGpuRequest),
    /// Stop any active session, depart virtual displays, and restore physical topology.
    RestoreTopology,
    /// Query service and driver health telemetry.
    GetHealth,
}

/// Tagged IPC response variants returned by `zerovdd-svc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcResponse {
    /// Generic acknowledgement without extra payload.
    Ok,
    /// Session started successfully with resulting status.
    SessionStarted(SessionStatus),
    /// Session stopped successfully.
    SessionStopped {
        /// Whether a saved physical display topology was restored.
        restored_topology: bool,
    },
    /// Current session status snapshot.
    SessionStatus(SessionStatus),
    /// Virtual display created or reused.
    DisplayAdded {
        /// Adapter LUID low part.
        adapter_luid_low: u32,
        /// Adapter LUID high part.
        adapter_luid_high: i32,
        /// VidPN target ID.
        target_id: u32,
        /// Internal connector slot index.
        connector_index: u32,
    },
    /// Virtual display removal result.
    DisplayRemoved {
        /// `true` if a matching display existed and was departed.
        removed: bool,
    },
    /// Virtual display mode updated in place.
    DisplayModeUpdated {
        /// Adapter LUID low part.
        adapter_luid_low: u32,
        /// Adapter LUID high part.
        adapter_luid_high: i32,
        /// VidPN target ID.
        target_id: u32,
        /// Internal connector slot index.
        connector_index: u32,
        /// Applied width in pixels.
        width: u32,
        /// Applied height in pixels.
        height: u32,
        /// Applied refresh rate in millihertz.
        refresh_mhz: u32,
        /// Active HDR state after update.
        hdr_active: bool,
        /// Number of modes published to IddCx.
        mode_count: u32,
    },
    /// Active virtual displays returned by [`IpcRequest::DisplayList`].
    DisplayList {
        /// Active virtual displays currently attached to the adapter.
        displays: Vec<ActiveDisplayInfo>,
    },
    /// Enumerated GPUs returned by [`IpcRequest::ListGpus`].
    GpuList {
        /// Available DXGI adapters.
        gpus: Vec<GpuAdapterSummary>,
        /// Currently configured GPU selector summary from `config.toml`.
        configured_selector: String,
        /// Whether the most recent asynchronous render GPU switch failed.
        last_gpu_switch_failed: bool,
    },
    /// Render GPU switched via [`IpcRequest::SwitchGpu`].
    GpuSwitched {
        /// Target adapter LUID low part.
        adapter_luid_low: u32,
        /// Target adapter LUID high part.
        adapter_luid_high: i32,
        /// Friendly description of the selected adapter.
        friendly_name: String,
    },
    /// Physical display topology restoration summary returned by [`IpcRequest::RestoreTopology`].
    TopologyRestored {
        /// `true` if restored from `topology-backup.json`; `false` if fallback re-enable ran.
        restored_from_backup: bool,
        /// Number of virtual displays departed prior to restoration.
        departed_displays: u32,
    },
    /// Service health snapshot.
    Health(ServiceHealth),
    /// Remote execution error returned by the service.
    Error(IpcErrorPayload),
}

fn validate_ipc_client_id(client_id: &str) -> Result<(), IpcError> {
    if client_id.trim().is_empty()
        || client_id.len() > MAX_IPC_CLIENT_ID_LEN
        || client_id.bytes().any(|b| b < 0x20 || b == 0x7F)
    {
        return Err(IpcError::InvalidClientId);
    }
    Ok(())
}

/// Encodes `msg` into a 4-byte little-endian length-prefixed JSON frame.
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, IpcError> {
    let payload = serde_json::to_vec(msg)?;
    if payload.is_empty() {
        return Err(IpcError::EmptyFrame);
    }
    if payload.len() > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            size: payload.len(),
        });
    }
    // WHY: payload.len() is bounded <= MAX_IPC_FRAME_BYTES (1 MiB), so cast to u32 cannot truncate.
    let len_u32 = payload.len() as u32;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len_u32.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decodes a single 4-byte little-endian length-prefixed JSON frame from `frame`.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, IpcError> {
    if frame.len() < 4 {
        return Err(IpcError::TruncatedHeader {
            actual: frame.len(),
        });
    }
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if len == 0 {
        return Err(IpcError::EmptyFrame);
    }
    if len > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge { size: len });
    }
    let actual_payload = frame.len() - 4;
    if actual_payload != len {
        return Err(IpcError::FrameLengthMismatch {
            declared: len,
            actual: actual_payload,
        });
    }
    Ok(serde_json::from_slice(&frame[4..])?)
}

/// Writes a length-prefixed JSON frame to a synchronous stream.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, msg: &T) -> Result<(), IpcError> {
    let frame = encode_frame(msg)?;
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Reads and validates a length-prefixed JSON frame from a synchronous stream.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, IpcError> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 {
        return Err(IpcError::EmptyFrame);
    }
    if len > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge { size: len });
    }
    // WHY: Validate declared frame size against MAX_IPC_FRAME_BYTES before allocating
    // so an untrusted peer cannot trigger an unbounded memory allocation.
    let mut payload = std::vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(serde_json::from_slice(&payload)?)
}
