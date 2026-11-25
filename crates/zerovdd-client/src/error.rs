//! Error types returned by `zerovdd-client` operations.

use thiserror::Error;

/// Errors produced by Control Device I/O, CCD topology planning, HDR management, and GPU selection.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Access to the privileged `zerovdd` Control Device was denied by the OS or driver ACL.
    #[error("access denied opening zerovdd Control Device (administrator privileges required)")]
    AccessDenied,

    /// No matching `zerovdd` Control Device interface or DOS symlink could be found.
    #[error("zerovdd Control Device not found ({0})")]
    DeviceNotFound(String),

    /// Target virtual display does not exist or has already departed in the driver.
    #[error("virtual display not found in driver")]
    MonitorNotFound,

    /// Driver IOCTL ABI major version is incompatible with this client build.
    #[error(
        "driver protocol major version mismatch: expected {expected_major}, got {actual_major}"
    )]
    ProtocolMismatch {
        /// Expected protocol major version compiled into `zerovdd-proto`.
        expected_major: u16,
        /// Actual protocol major version reported by the driver.
        actual_major: u16,
    },

    /// Wire payload encoding, decoding, or validation failed.
    #[error("protocol error: {0}")]
    Proto(#[from] zerovdd_proto::validation::ProtoError),

    /// Configuration schema or persistence error.
    #[error("configuration error: {0}")]
    Config(#[from] zerovdd_config::ConfigError),

    /// Filesystem or device I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Topology snapshot JSON serialization or deserialization failed.
    #[error("snapshot JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Caller supplied invalid or contradictory parameters.
    #[error("invalid argument: {0}")]
    InvalidInput(String),

    /// Target virtual display did not appear in Windows CCD within the configured retry budget.
    #[error("virtual display ({adapter_luid_low:#x}:{adapter_luid_high:#x}, target {target_id}) not found in CCD")]
    VirtualDisplayNotFound {
        /// Low 32 bits of the virtual adapter LUID.
        adapter_luid_low: u32,
        /// High 32 bits of the virtual adapter LUID.
        adapter_luid_high: i32,
        /// OS VidPN target identifier returned by `IOCTL_ADD_DISPLAY`.
        target_id: u32,
    },

    /// Restoring physical displays failed because zero active physical monitors remain available.
    #[error("no available physical displays remain to restore an active desktop topology")]
    NoActivePhysicalDisplays,

    /// Requested HDR mode is not supported by the target display or current OS policy.
    #[error("HDR unsupported on target {target_id}: {reason}")]
    HdrUnsupported {
        /// VidPN target ID of the display.
        target_id: u32,
        /// Human-readable reason explaining why HDR cannot be enabled.
        reason: String,
    },

    /// No GPU adapter matched the requested selector.
    #[error("no matching GPU adapter found for selector ({query})")]
    GpuNotFound {
        /// Description of the requested GPU selector.
        query: String,
    },

    /// Snapshot file could not be validated or uses an unsupported schema version.
    #[error("corrupt or unsupported topology snapshot: {0}")]
    SnapshotCorrupt(String),

    /// Win32 API call failed with a non-zero error code.
    #[error("{operation} failed with Win32 error {code:#010x}")]
    Win32 {
        /// Name of the failing Win32 operation.
        operation: &'static str,
        /// Win32 error code (`GetLastError` / `CONFIGRET` / `HRESULT`).
        code: u32,
    },

    /// Windows CCD (`QueryDisplayConfig` / `SetDisplayConfig` / `DisplayConfig*`) API call failed.
    #[error("{operation} failed with status {status}")]
    Ccd {
        /// Name of the failing CCD function.
        operation: &'static str,
        /// Returned Win32 status code.
        status: i32,
    },
}
