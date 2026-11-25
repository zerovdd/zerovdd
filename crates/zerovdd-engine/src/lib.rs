//! `#![no_std]` re-entrancy-safe driver state engine and deterministic IddCx simulator.
//!
//! `zerovdd-engine` implements the monitor registry, hotplug lifecycle,
//! per-handle and leased ownership watchdog, D0/D3 power-state idempotency,
//! GPU-switch circuit breaker, and typed IOCTL dispatcher without any Windows or
//! WDK dependencies.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

/// Typed IOCTL dispatcher bridging wire requests to [`VddEngine`] operations.
pub mod dispatch;
/// Adapter power-state idempotency and GPU-switch circuit breaker.
pub mod gpu;
/// Core virtual display lifecycle orchestration and re-entrant callback handlers.
pub mod lifecycle;
/// Mockable UMDF 2 / IddCx 1.10 platform trait and deterministic simulator.
pub mod platform;
/// Re-entrancy-safe monitor slot table and specification store.
pub mod registry;
/// Lease expiration, renewal, and per-handle cleanup watchdog.
pub mod watchdog;

use thiserror::Error;
use zerovdd_edid::EdidError;
use zerovdd_modes::ModeError;
use zerovdd_proto::validation::ProtoError;

pub use dispatch::dispatch_ioctl;
pub use gpu::{
    GpuController, GpuSnapshot, GpuSwitchError, PendingGpuSwitch, SwapchainAssignAction,
    DEFAULT_MAX_GPU_SWITCH_ATTEMPTS,
};
pub use lifecycle::{ParseMonitorResult, TargetModesResult, VddEngine};
pub use platform::{
    AdapterLuid, AdapterState, ArriveMonitorContext, ConnectorSlot, FakeIddPlatform, FileKey,
    IddPlatform, MonitorArrivalResult, MonitorHandle, PlatformError, MAX_MONITORS,
};
pub use registry::{MonitorQueryKey, MonitorRegistry, MonitorSpec};

/// Errors returned by [`VddEngine`] lifecycle, validation, and dispatch operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EngineError {
    /// Wire protocol decode or semantic validation failed.
    #[error("protocol validation error: {0}")]
    Proto(#[from] ProtoError),
    /// EDID synthesis or custom EDID validation failed.
    #[error("EDID error: {0}")]
    Edid(#[from] EdidError),
    /// Display mode planning or timing math failed.
    #[error("mode planning error: {0}")]
    Mode(#[from] ModeError),
    /// Underlying platform adapter operation failed.
    #[error("platform error: {0}")]
    Platform(#[from] PlatformError),
    /// All virtual connector slots are currently occupied.
    #[error("all {max} virtual monitor slots are in use")]
    NoFreeConnectorSlots {
        /// Maximum simultaneous virtual monitors supported by the adapter.
        max: usize,
    },
    /// Monitor GUID is already active or pending under a different client handle.
    #[error("monitor GUID is already owned by another active handle")]
    MonitorConflict,
    /// Requested virtual monitor does not exist in the registry.
    #[error("virtual monitor not found")]
    MonitorNotFound,
    /// Requested operation requires a leased display rather than a handle-bound display.
    #[error("display is handle-bound and does not have a renewable lease")]
    NotLeased,
    /// Leased virtual display has already passed its expiration timestamp.
    #[error("display lease has expired")]
    LeaseExpired,
    /// Display arrival was cancelled by a concurrent removal or file-handle cleanup.
    #[error("display arrival was cancelled by concurrent removal or handle cleanup")]
    CancelledDuringArrival,
    /// Requested dimensions exceed 16-bit EDID descriptor bounds.
    #[error("display dimensions {width}x{height} exceed EDID bounds")]
    DimensionsOutOfRange {
        /// Requested display width in pixels.
        width: u32,
        /// Requested display height in lines.
        height: u32,
    },
    /// IOCTL control code is unrecognized by the engine dispatcher.
    #[error("unsupported IOCTL code {0:#010x}")]
    UnsupportedIoctl(u32),
}
