//! Mockable UMDF 2 / IddCx 1.10 platform trait and deterministic simulator.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use thiserror::Error;

/// Maximum number of concurrent virtual monitors supported by the adapter.
pub const MAX_MONITORS: usize = 16;

/// Opaque token identifying an open Control Device file handle (`WDFFILEOBJECT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileKey(pub u64);

/// Portable representation of a Windows adapter `LUID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct AdapterLuid {
    /// Low 32 bits of the locally unique identifier.
    pub low: u32,
    /// High 32 bits of the locally unique identifier.
    pub high: i32,
}

impl AdapterLuid {
    /// Zero/unset `LUID` requesting automatic system adapter selection.
    pub const ZERO: Self = Self { low: 0, high: 0 };

    /// Constructs an [`AdapterLuid`] from its low and high components.
    #[must_use]
    pub const fn new(low: u32, high: i32) -> Self {
        Self { low, high }
    }
}

/// Opaque handle representing an `IDDCX_MONITOR` instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorHandle(pub u64);

/// Zero-based virtual connector slot index (`0..MAX_MONITORS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectorSlot(pub u32);

/// Power and initialization lifecycle state of the indirect display adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdapterState {
    /// Adapter has not yet received its initial `D0Entry`.
    #[default]
    Uninitialized,
    /// `IddCxAdapterInitAsync` has been dispatched and is in flight.
    Initializing,
    /// Adapter is initialized and active in `D0`.
    Ready,
    /// Adapter entered `D3` (`D0Exit`) and may resume without re-initializing.
    Suspended,
}

/// Metadata returned by [`IddPlatform::arrive_monitor`] upon successful PnP arrival.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorArrivalResult {
    /// Windows `LUID` of the indirect display adapter exposing this target.
    pub adapter_luid: AdapterLuid,
    /// OS VidPN target identifier assigned to the arrived monitor.
    pub target_id: u32,
}

/// Platform errors returned by [`IddPlatform`] implementations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlatformError {
    /// Adapter initialization (`IddCxAdapterInitAsync`) failed.
    #[error("adapter initialization failed with status {code:#010x}")]
    InitAdapterFailed {
        /// Platform failure status code.
        code: i32,
    },
    /// Monitor object creation (`IddCxMonitorCreate`) failed.
    #[error("monitor creation failed on slot {slot} with status {code:#010x}")]
    CreateMonitorFailed {
        /// Target connector slot index.
        slot: u32,
        /// Platform failure status code.
        code: i32,
    },
    /// Monitor arrival (`IddCxMonitorArrival`) failed.
    #[error("monitor arrival failed on slot {slot} with status {code:#010x}")]
    ArriveMonitorFailed {
        /// Target connector slot index.
        slot: u32,
        /// Platform failure status code.
        code: i32,
    },
    /// Monitor departure (`IddCxMonitorDeparture`) failed.
    #[error("monitor departure failed for handle {handle} with status {code:#010x}")]
    DepartMonitorFailed {
        /// Opaque monitor handle that failed to depart.
        handle: u64,
        /// Platform failure status code.
        code: i32,
    },
    /// Render adapter update (`IddCxAdapterSetRenderAdapter`) failed.
    #[error("set render adapter failed for LUID ({low:#x}, {high}) with status {code:#010x}")]
    SetRenderAdapterFailed {
        /// Requested adapter LUID low part.
        low: u32,
        /// Requested adapter LUID high part.
        high: i32,
        /// Platform failure status code.
        code: i32,
    },
    /// Monitor target mode list update (`IddCxMonitorUpdateModes2`) failed.
    #[error("update monitor modes failed for handle {handle} with status {code:#010x}")]
    UpdateMonitorModesFailed {
        /// Opaque monitor handle whose modes failed to update.
        handle: u64,
        /// Platform failure status code.
        code: i32,
    },
}

/// Abstraction over UMDF 2 / IddCx 1.10 adapter and monitor side effects.
pub trait IddPlatform: Send + Sync {
    /// Initializes the indirect display adapter (`IddCxAdapterInitAsync`).
    fn init_adapter(&self) -> Result<(), PlatformError> {
        Ok(())
    }

    /// Creates a monitor object for `slot` with `monitor_guid` and `edid`.
    fn create_monitor(
        &self,
        slot: ConnectorSlot,
        monitor_guid: [u8; 16],
        edid: &[u8],
    ) -> Result<MonitorHandle, PlatformError>;

    /// Triggers PnP arrival for `handle`; implementations may synchronously re-enter
    /// monitor description and target mode callbacks on the calling thread.
    fn arrive_monitor(
        &self,
        handle: MonitorHandle,
        slot: ConnectorSlot,
        monitor_guid: [u8; 16],
    ) -> Result<MonitorArrivalResult, PlatformError>;

    /// Triggers PnP departure and tears down `handle`.
    fn depart_monitor(&self, handle: MonitorHandle) -> Result<(), PlatformError>;

    /// Updates the OS-visible target modes for an active monitor via `IddCxMonitorUpdateModes[2]`.
    fn update_monitor_modes(
        &self,
        _handle: MonitorHandle,
        _planned_modes: &[zerovdd_modes::PlannedMode],
    ) -> Result<(), PlatformError> {
        Ok(())
    }

    /// Requests switching the active render GPU adapter to `luid`.
    fn set_render_adapter(&self, luid: AdapterLuid) -> Result<(), PlatformError>;

    /// Returns the current monotonic timestamp in milliseconds.
    fn now_monotonic_ms(&self) -> u64;
}

/// Context passed to a synchronous [`FakeIddPlatform`] arrival callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArriveMonitorContext {
    /// Opaque handle created for the arriving monitor.
    pub monitor_handle: MonitorHandle,
    /// Connector slot assigned to the monitor.
    pub connector_slot: ConnectorSlot,
    /// Stable monitor container GUID.
    pub monitor_guid: [u8; 16],
    /// Raw EDID payload provided to [`IddPlatform::create_monitor`].
    pub edid: Vec<u8>,
}

type ArriveHook = Arc<dyn Fn(&ArriveMonitorContext) -> Result<(), PlatformError> + Send + Sync>;

#[derive(Debug, Clone)]
struct SimMonitorRecord {
    slot: ConnectorSlot,
    monitor_guid: [u8; 16],
    edid: Vec<u8>,
    arrived: bool,
    updated_modes: Vec<zerovdd_modes::PlannedMode>,
}

struct FakeState {
    next_handle: u64,
    next_target_id: u32,
    monitors: BTreeMap<MonitorHandle, SimMonitorRecord>,
    current_render_adapter: AdapterLuid,
    arrive_hook: Option<ArriveHook>,
}

/// Deterministic in-memory [`IddPlatform`] simulator with fault injection,
/// synchronous arrival re-entrancy hooks, and a monotonic clock.
pub struct FakeIddPlatform {
    adapter_luid: AdapterLuid,
    now_ms: AtomicU64,
    fail_init_adapter: AtomicBool,
    fail_create_monitor: AtomicBool,
    fail_arrive_monitor: AtomicBool,
    fail_depart_monitor: AtomicBool,
    fail_update_monitor_modes: AtomicBool,
    fail_set_render_adapter: AtomicBool,
    init_adapter_calls: AtomicU64,
    create_monitor_calls: AtomicU64,
    arrive_monitor_calls: AtomicU64,
    depart_monitor_calls: AtomicU64,
    update_monitor_modes_calls: AtomicU64,
    set_render_adapter_calls: AtomicU64,
    state: Mutex<FakeState>,
}

impl FakeIddPlatform {
    /// Creates a new simulator with default adapter LUID `(0x1000, 0)`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_adapter_luid(AdapterLuid::new(0x1000, 0))
    }

    /// Creates a new simulator reporting the specified adapter `LUID`.
    #[must_use]
    pub fn with_adapter_luid(adapter_luid: AdapterLuid) -> Self {
        Self {
            adapter_luid,
            now_ms: AtomicU64::new(0),
            fail_init_adapter: AtomicBool::new(false),
            fail_create_monitor: AtomicBool::new(false),
            fail_arrive_monitor: AtomicBool::new(false),
            fail_depart_monitor: AtomicBool::new(false),
            fail_update_monitor_modes: AtomicBool::new(false),
            fail_set_render_adapter: AtomicBool::new(false),
            init_adapter_calls: AtomicU64::new(0),
            create_monitor_calls: AtomicU64::new(0),
            arrive_monitor_calls: AtomicU64::new(0),
            depart_monitor_calls: AtomicU64::new(0),
            update_monitor_modes_calls: AtomicU64::new(0),
            set_render_adapter_calls: AtomicU64::new(0),
            state: Mutex::new(FakeState {
                next_handle: 1,
                next_target_id: 100,
                monitors: BTreeMap::new(),
                current_render_adapter: AdapterLuid::ZERO,
                arrive_hook: None,
            }),
        }
    }

    /// Registers a synchronous callback invoked during [`IddPlatform::arrive_monitor`].
    pub fn set_arrive_hook<F>(&self, hook: F)
    where
        F: Fn(&ArriveMonitorContext) -> Result<(), PlatformError> + Send + Sync + 'static,
    {
        let mut guard = self.state.lock();
        guard.arrive_hook = Some(Arc::new(hook));
    }

    /// Removes any previously registered arrival callback.
    pub fn clear_arrive_hook(&self) {
        let mut guard = self.state.lock();
        guard.arrive_hook = None;
    }

    /// Configures fault injection for [`IddPlatform::init_adapter`].
    pub fn set_fail_init_adapter(&self, fail: bool) {
        self.fail_init_adapter.store(fail, Ordering::SeqCst);
    }

    /// Configures fault injection for [`IddPlatform::create_monitor`].
    pub fn set_fail_create_monitor(&self, fail: bool) {
        self.fail_create_monitor.store(fail, Ordering::SeqCst);
    }

    /// Configures fault injection for [`IddPlatform::arrive_monitor`].
    pub fn set_fail_arrive_monitor(&self, fail: bool) {
        self.fail_arrive_monitor.store(fail, Ordering::SeqCst);
    }

    /// Configures fault injection for [`IddPlatform::depart_monitor`].
    pub fn set_fail_depart_monitor(&self, fail: bool) {
        self.fail_depart_monitor.store(fail, Ordering::SeqCst);
    }

    /// Configures fault injection for [`IddPlatform::update_monitor_modes`].
    pub fn set_fail_update_monitor_modes(&self, fail: bool) {
        self.fail_update_monitor_modes.store(fail, Ordering::SeqCst);
    }

    /// Configures fault injection for [`IddPlatform::set_render_adapter`].
    pub fn set_fail_set_render_adapter(&self, fail: bool) {
        self.fail_set_render_adapter.store(fail, Ordering::SeqCst);
    }

    /// Advances the simulated monotonic clock by `delta_ms` and returns the new timestamp.
    pub fn advance_ms(&self, delta_ms: u64) -> u64 {
        self.now_ms
            .fetch_add(delta_ms, Ordering::SeqCst)
            .saturating_add(delta_ms)
    }

    /// Sets the simulated monotonic clock to `now_ms`.
    pub fn set_now_ms(&self, now_ms: u64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Returns how many times [`IddPlatform::init_adapter`] was called.
    #[must_use]
    pub fn init_adapter_calls(&self) -> u64 {
        self.init_adapter_calls.load(Ordering::SeqCst)
    }

    /// Returns how many times [`IddPlatform::create_monitor`] was called.
    #[must_use]
    pub fn create_monitor_calls(&self) -> u64 {
        self.create_monitor_calls.load(Ordering::SeqCst)
    }

    /// Returns how many times [`IddPlatform::arrive_monitor`] was called.
    #[must_use]
    pub fn arrive_monitor_calls(&self) -> u64 {
        self.arrive_monitor_calls.load(Ordering::SeqCst)
    }

    /// Returns how many times [`IddPlatform::depart_monitor`] was called.
    #[must_use]
    pub fn depart_monitor_calls(&self) -> u64 {
        self.depart_monitor_calls.load(Ordering::SeqCst)
    }

    /// Returns how many times [`IddPlatform::update_monitor_modes`] was called.
    #[must_use]
    pub fn update_monitor_modes_calls(&self) -> u64 {
        self.update_monitor_modes_calls.load(Ordering::SeqCst)
    }

    /// Returns the number of modes stored by the latest [`IddPlatform::update_monitor_modes`] call.
    #[must_use]
    pub fn monitor_updated_mode_count(&self, handle: MonitorHandle) -> Option<usize> {
        self.state
            .lock()
            .monitors
            .get(&handle)
            .map(|rec| rec.updated_modes.len())
    }

    /// Returns how many times [`IddPlatform::set_render_adapter`] was called.
    #[must_use]
    pub fn set_render_adapter_calls(&self) -> u64 {
        self.set_render_adapter_calls.load(Ordering::SeqCst)
    }

    /// Returns the number of monitor handles created and not yet departed.
    #[must_use]
    pub fn created_monitor_count(&self) -> usize {
        self.state.lock().monitors.len()
    }

    /// Returns the number of monitors currently marked as arrived in the simulator.
    #[must_use]
    pub fn arrived_monitor_count(&self) -> usize {
        self.state
            .lock()
            .monitors
            .values()
            .filter(|rec| rec.arrived)
            .count()
    }

    /// Returns the most recent render adapter LUID passed to [`IddPlatform::set_render_adapter`].
    #[must_use]
    pub fn current_render_adapter(&self) -> AdapterLuid {
        self.state.lock().current_render_adapter
    }
}

impl Default for FakeIddPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl IddPlatform for FakeIddPlatform {
    fn init_adapter(&self) -> Result<(), PlatformError> {
        self.init_adapter_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_init_adapter.load(Ordering::SeqCst) {
            return Err(PlatformError::InitAdapterFailed {
                code: -1_073_741_823_i32,
            });
        }
        Ok(())
    }

    fn create_monitor(
        &self,
        slot: ConnectorSlot,
        monitor_guid: [u8; 16],
        edid: &[u8],
    ) -> Result<MonitorHandle, PlatformError> {
        self.create_monitor_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_create_monitor.load(Ordering::SeqCst) {
            return Err(PlatformError::CreateMonitorFailed {
                slot: slot.0,
                code: -1_073_741_670_i32,
            });
        }

        let mut guard = self.state.lock();
        let handle = MonitorHandle(guard.next_handle);
        guard.next_handle = guard.next_handle.saturating_add(1);
        guard.monitors.insert(
            handle,
            SimMonitorRecord {
                slot,
                monitor_guid,
                edid: edid.to_vec(),
                arrived: false,
                updated_modes: Vec::new(),
            },
        );
        Ok(handle)
    }

    fn arrive_monitor(
        &self,
        handle: MonitorHandle,
        slot: ConnectorSlot,
        monitor_guid: [u8; 16],
    ) -> Result<MonitorArrivalResult, PlatformError> {
        self.arrive_monitor_calls.fetch_add(1, Ordering::SeqCst);

        let (hook, edid) = {
            let guard = self.state.lock();
            let rec = guard
                .monitors
                .get(&handle)
                .filter(|rec| rec.slot == slot && rec.monitor_guid == monitor_guid)
                .ok_or(PlatformError::ArriveMonitorFailed {
                    slot: slot.0,
                    code: -1_073_741_811_i32,
                })?;
            (guard.arrive_hook.clone(), rec.edid.clone())
        };

        // WHY: Clone the hook and drop FakeState's mutex before invoking it so synchronous
        // re-entrant callbacks into VddEngine cannot deadlock against FakeIddPlatform.
        if let Some(cb) = hook {
            let ctx = ArriveMonitorContext {
                monitor_handle: handle,
                connector_slot: slot,
                monitor_guid,
                edid,
            };
            cb(&ctx)?;
        }

        if self.fail_arrive_monitor.load(Ordering::SeqCst) {
            return Err(PlatformError::ArriveMonitorFailed {
                slot: slot.0,
                code: -1_073_741_823_i32,
            });
        }

        let mut guard = self.state.lock();
        let target_id = guard.next_target_id;
        guard.next_target_id = guard.next_target_id.saturating_add(1);
        if let Some(rec) = guard.monitors.get_mut(&handle) {
            rec.arrived = true;
        } else {
            return Err(PlatformError::ArriveMonitorFailed {
                slot: slot.0,
                code: -1_073_741_811_i32,
            });
        }

        Ok(MonitorArrivalResult {
            adapter_luid: self.adapter_luid,
            target_id,
        })
    }

    fn depart_monitor(&self, handle: MonitorHandle) -> Result<(), PlatformError> {
        self.depart_monitor_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_depart_monitor.load(Ordering::SeqCst) {
            return Err(PlatformError::DepartMonitorFailed {
                handle: handle.0,
                code: -1_073_741_823_i32,
            });
        }

        let mut guard = self.state.lock();
        if guard.monitors.remove(&handle).is_none() {
            return Err(PlatformError::DepartMonitorFailed {
                handle: handle.0,
                code: -1_073_741_811_i32,
            });
        }
        Ok(())
    }

    fn update_monitor_modes(
        &self,
        handle: MonitorHandle,
        planned_modes: &[zerovdd_modes::PlannedMode],
    ) -> Result<(), PlatformError> {
        self.update_monitor_modes_calls
            .fetch_add(1, Ordering::SeqCst);
        if self.fail_update_monitor_modes.load(Ordering::SeqCst) {
            return Err(PlatformError::UpdateMonitorModesFailed {
                handle: handle.0,
                code: -1_073_741_823_i32,
            });
        }
        let mut guard = self.state.lock();
        let Some(rec) = guard.monitors.get_mut(&handle) else {
            return Err(PlatformError::UpdateMonitorModesFailed {
                handle: handle.0,
                code: -1_073_741_811_i32,
            });
        };
        rec.updated_modes = planned_modes.to_vec();
        Ok(())
    }

    fn set_render_adapter(&self, luid: AdapterLuid) -> Result<(), PlatformError> {
        self.set_render_adapter_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_set_render_adapter.load(Ordering::SeqCst) {
            return Err(PlatformError::SetRenderAdapterFailed {
                low: luid.low,
                high: luid.high,
                code: -1_073_741_811_i32,
            });
        }
        let mut guard = self.state.lock();
        guard.current_render_adapter = luid;
        Ok(())
    }

    fn now_monotonic_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}
