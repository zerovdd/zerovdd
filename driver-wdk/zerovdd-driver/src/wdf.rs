//! WDF driver/device lifecycle, `Arc`-managed runtime ownership, lease timer, and context accessors.

use alloc::sync::Arc;
use core::ffi::{c_void, CStr};
use core::mem::size_of;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use spin::Mutex;
use wdk_sys::call_unsafe_wdf_function_binding;
use wdk_sys::{
    NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, WDFCMRESLIST, WDFDEVICE, WDFDRIVER, WDFFILEOBJECT,
    WDFOBJECT, WDFTIMER, WDF_DRIVER_CONFIG, WDF_FILEOBJECT_CONFIG, WDF_OBJECT_ATTRIBUTES,
    WDF_OBJECT_CONTEXT_TYPE_INFO, WDF_PNPPOWER_EVENT_CALLBACKS, WDF_POWER_DEVICE_STATE,
    WDF_TIMER_CONFIG,
};
use zerovdd_engine::{FileKey, MonitorQueryKey, ParseMonitorResult, VddEngine};
use zerovdd_iddcx_sys::{
    zerovdd_iddcx_device_init_config, zerovdd_iddcx_device_initialize, zerovdd_iddcx_get_version,
    zerovdd_iddcx_probe_v110, zerovdd_iddcx_supports_fp16, NtStatus, WdfDevicePtr,
};
use zerovdd_proto::types::RemoveDisplayRequest;

use crate::control_device::{
    control_reference_unicode_string, evt_device_file_create, evt_file_cleanup, evt_file_close,
    evt_iddcx_device_io_control, FileContext, CONTROL_INTERFACE_GUID,
};
use crate::d3d::D3D11DeviceManager;
use crate::etw::EtwLogger;
use crate::iddcx::{build_callbacks, WdfIddPlatform};
use crate::swapchain::SwapChainRegistry;

/// `NTSTATUS` success (`0`).
pub const STATUS_SUCCESS: NTSTATUS = 0;
/// `NTSTATUS` generic failure (`0xC0000001`).
pub const STATUS_UNSUCCESSFUL: NTSTATUS = 0xC000_0001_u32 as i32;
/// `NTSTATUS` invalid handle (`0xC0000008`).
pub const STATUS_INVALID_HANDLE: NTSTATUS = 0xC000_0008_u32 as i32;
/// `NTSTATUS` invalid parameter (`0xC000000D`).
pub const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000D_u32 as i32;
/// `NTSTATUS` invalid device request (`0xC0000010`).
pub const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010_u32 as i32;
/// `NTSTATUS` access denied (`0xC0000022`).
pub const STATUS_ACCESS_DENIED: NTSTATUS = 0xC000_0022_u32 as i32;
/// `NTSTATUS` buffer too small (`0xC0000023`).
pub const STATUS_BUFFER_TOO_SMALL: NTSTATUS = 0xC000_0023_u32 as i32;
/// `NTSTATUS` object name collision (`0xC0000035`).
pub const STATUS_OBJECT_NAME_COLLISION: NTSTATUS = 0xC000_0035_u32 as i32;
/// `NTSTATUS` insufficient resources (`0xC000009A`).
pub const STATUS_INSUFFICIENT_RESOURCES: NTSTATUS = 0xC000_009A_u32 as i32;
/// `NTSTATUS` cancelled (`0xC0000120`).
pub const STATUS_CANCELLED: NTSTATUS = 0xC000_0120_u32 as i32;
/// `NTSTATUS` not found (`0xC0000225`).
pub const STATUS_NOT_FOUND: NTSTATUS = 0xC000_0225_u32 as i32;
/// `NTSTATUS` no data detected (`0x80000022`).
pub const STATUS_NO_DATA_DETECTED: NTSTATUS = 0x8000_0022_u32 as i32;
/// `NTSTATUS` requesting swapchain abandonment for GPU adapter switch (`0xC01E0012`).
pub const STATUS_GRAPHICS_INDIRECT_DISPLAY_ABANDON_SWAPCHAIN: NTSTATUS = 0xC01E_0012_u32 as i32;

const MAX_ACTIVE_DEVICES: usize = 4;
const LEASE_REAP_PERIOD_MS: u32 = 1000;
const LEASE_REAP_DUE_TIME_100NS: i64 = -10_000 * (LEASE_REAP_PERIOD_MS as i64);

// WHY: Store `Arc<DeviceRuntime>` references in a bounded table protected by `spin::Mutex`
// so `EvtIddCxParseMonitorDescription[2]`, which has no `WDFDEVICE` parameter, can safely
// keep `DeviceRuntime` alive via `Arc` without fabricating `'static` references; entries are
// cleared deterministically in `EvtDeviceReleaseHardware`.
static ACTIVE_RUNTIMES: Mutex<[Option<Arc<DeviceRuntime>>; MAX_ACTIVE_DEVICES]> =
    Mutex::new([const { None }; MAX_ACTIVE_DEVICES]);

/// Device-scoped driver runtime owned via `Arc` in `DeviceContext` and torn down in `ReleaseHardware`.
pub struct DeviceRuntime {
    /// Owning `WDFDEVICE` handle.
    pub device: AtomicPtr<c_void>,
    /// `IDDCX_ADAPTER` handle assigned by `IddCxAdapterInitAsync`.
    pub adapter: AtomicPtr<c_void>,
    /// Runtime IddCx 1.10 capability bitmask returned by `zerovdd_iddcx_probe_v110`.
    pub iddcx_caps: AtomicU32,
    /// Pure-Rust re-entrancy-safe virtual display state machine.
    pub engine: VddEngine,
    /// Device-scoped D3D11 / DXGI device cache (zero static COM state).
    pub d3d: Arc<D3D11DeviceManager>,
    /// Per-monitor swapchain worker registry.
    pub workers: SwapChainRegistry,
    /// Structured ETW TraceLogging provider (`ZeroVdd.Driver`).
    pub etw: Arc<EtwLogger>,
}

#[repr(C)]
struct DeviceContext {
    runtime: Mutex<Option<Arc<DeviceRuntime>>>,
    lease_timer: AtomicPtr<c_void>,
}

#[repr(transparent)]
struct ContextTypeInfoWrapper(WDF_OBJECT_CONTEXT_TYPE_INFO);

// SAFETY: `WDF_OBJECT_CONTEXT_TYPE_INFO` holds only immutable metadata pointers and sizes
// consumed by WDF's context registration runtime.
unsafe impl Sync for ContextTypeInfoWrapper {}

const DEVICE_CONTEXT_NAME: &CStr = c"DeviceContext";
const FILE_CONTEXT_NAME: &CStr = c"FileContext";

static DEVICE_CONTEXT_TYPE_INFO: ContextTypeInfoWrapper =
    ContextTypeInfoWrapper(WDF_OBJECT_CONTEXT_TYPE_INFO {
        Size: size_of::<WDF_OBJECT_CONTEXT_TYPE_INFO>() as u32,
        ContextName: DEVICE_CONTEXT_NAME.as_ptr(),
        ContextSize: size_of::<DeviceContext>(),
        UniqueType: ptr::null(),
        EvtDriverGetUniqueContextType: None,
    });

static FILE_CONTEXT_TYPE_INFO: ContextTypeInfoWrapper =
    ContextTypeInfoWrapper(WDF_OBJECT_CONTEXT_TYPE_INFO {
        Size: size_of::<WDF_OBJECT_CONTEXT_TYPE_INFO>() as u32,
        ContextName: FILE_CONTEXT_NAME.as_ptr(),
        ContextSize: size_of::<FileContext>(),
        UniqueType: ptr::null(),
        EvtDriverGetUniqueContextType: None,
    });

fn init_object_attributes(
    context_info: &'static ContextTypeInfoWrapper,
    cleanup_cb: wdk_sys::PFN_WDF_OBJECT_CONTEXT_CLEANUP,
) -> WDF_OBJECT_ATTRIBUTES {
    WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<WDF_OBJECT_ATTRIBUTES>() as u32,
        EvtCleanupCallback: cleanup_cb,
        EvtDestroyCallback: None,
        ExecutionLevel: wdk_sys::_WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope:
            wdk_sys::_WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ParentObject: ptr::null_mut(),
        ContextSizeOverride: 0,
        ContextTypeInfo: &context_info.0,
    }
}

fn register_active_runtime(runtime: Arc<DeviceRuntime>) {
    let target_dev = runtime.device.load(Ordering::Acquire);
    if target_dev.is_null() {
        return;
    }
    let mut guard = ACTIVE_RUNTIMES.lock();
    for existing in guard.iter().flatten() {
        if existing.device.load(Ordering::Acquire) == target_dev {
            return;
        }
    }
    for slot in guard.iter_mut() {
        if slot.is_none() {
            *slot = Some(runtime);
            return;
        }
    }
}

fn unregister_active_runtime(device: WdfDevicePtr) {
    if device.is_null() {
        return;
    }
    let mut guard = ACTIVE_RUNTIMES.lock();
    for slot in guard.iter_mut() {
        let matches = slot
            .as_ref()
            .is_some_and(|rt| rt.device.load(Ordering::Acquire) == device);
        if matches {
            *slot = None;
        }
    }
}

/// Looks up a monitor specification by EDID across all active `WDFDEVICE` runtimes.
pub fn find_runtime_for_edid_lookup(edid: &[u8]) -> Option<ParseMonitorResult> {
    let snapshot: [Option<Arc<DeviceRuntime>>; MAX_ACTIVE_DEVICES] = {
        let guard = ACTIVE_RUNTIMES.lock();
        guard.clone()
    };
    for runtime in snapshot.into_iter().flatten() {
        if let Ok(res) = runtime
            .engine
            .on_parse_monitor_description(MonitorQueryKey::Edid(edid))
        {
            return Some(res);
        }
    }
    None
}

// SAFETY: `device` must be a valid `WDFDEVICE` created with `DEVICE_CONTEXT_TYPE_INFO`.
unsafe fn get_device_context(device: WDFDEVICE) -> *mut DeviceContext {
    // SAFETY: Caller guarantees `device` carries a `DeviceContext` allocation.
    let raw = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfObjectGetTypedContextWorker,
            device.cast::<c_void>(),
            &DEVICE_CONTEXT_TYPE_INFO.0,
        )
    };
    raw.cast::<DeviceContext>()
}

/// Clones an `Arc<DeviceRuntime>` for `device` while the device runtime remains active.
pub fn get_device_runtime(device: WdfDevicePtr) -> Option<Arc<DeviceRuntime>> {
    if device.is_null() {
        return None;
    }
    // SAFETY: `device` is a valid `WDFDEVICE` carrying `DeviceContext`.
    let ctx = unsafe { get_device_context(device.cast()) };
    if ctx.is_null() {
        return None;
    }
    // SAFETY: `ctx` points to initialized `DeviceContext` memory owned by WDF until `ContextCleanup`.
    let guard = unsafe { (*ctx).runtime.lock() };
    guard.as_ref().map(Arc::clone)
}

/// Returns the mutable [`FileContext`] pointer associated with `file_object`.
///
/// # Safety
///
/// `file_object` must be a valid `WDFFILEOBJECT` created with `FILE_CONTEXT_TYPE_INFO`.
// SAFETY: Caller guarantees `file_object` carries a `FileContext` allocation.
pub unsafe fn get_file_context(file_object: WDFFILEOBJECT) -> *mut FileContext {
    // SAFETY: Caller guarantees `file_object` carries a `FileContext` allocation.
    let raw = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfObjectGetTypedContextWorker,
            file_object.cast::<c_void>(),
            &FILE_CONTEXT_TYPE_INFO.0,
        )
    };
    raw.cast::<FileContext>()
}

fn stop_lease_timer(device: WDFDEVICE, clear_handle: bool) {
    // SAFETY: `device` carries a `DeviceContext`.
    let ctx = unsafe { get_device_context(device) };
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` points to valid `DeviceContext` memory owned by WDF.
    let timer_ptr = unsafe {
        if clear_handle {
            (*ctx).lease_timer.swap(ptr::null_mut(), Ordering::AcqRel)
        } else {
            (*ctx).lease_timer.load(Ordering::Acquire)
        }
    };
    if !timer_ptr.is_null() {
        // SAFETY: `timer_ptr` is a valid `WDFTIMER` parented to `device`; `Wait = 1`
        // waits synchronously until any running `EvtTimerFunc` completes.
        unsafe {
            let _stopped = call_unsafe_wdf_function_binding!(WdfTimerStop, timer_ptr.cast(), 1);
        }
    }
}

fn teardown_device_runtime(device: WDFDEVICE, stop_timer: bool) {
    if stop_timer {
        stop_lease_timer(device, true);
    } else {
        // SAFETY: `device` carries `DeviceContext`; in `EvtCleanupCallback`, WDF has already
        // stopped and deleted child `WDFTIMER` objects bottom-up before invoking this callback.
        let ctx = unsafe { get_device_context(device) };
        if !ctx.is_null() {
            // SAFETY: Clear pointer without touching the already-deleted `WDFTIMER` handle.
            unsafe {
                (*ctx).lease_timer.store(ptr::null_mut(), Ordering::Release);
            }
        }
    }
    unregister_active_runtime(device.cast());

    // SAFETY: `device` carries a `DeviceContext`.
    let ctx = unsafe { get_device_context(device) };
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` points to initialized `DeviceContext` memory owned by WDF.
    let maybe_runtime = unsafe {
        let mut guard = (*ctx).runtime.lock();
        guard.take()
    };
    let Some(runtime) = maybe_runtime else {
        return;
    };

    // WHY: Stop and join all swapchain worker threads first so no worker touches
    // `D3D11DeviceManager` or `IDDCX_MONITOR` during departure and COM teardown.
    runtime.workers.stop_all();

    let platform = WdfIddPlatform::new(&runtime);
    let snapshot = runtime.engine.list_displays(&platform);
    let count = usize::try_from(snapshot.count).unwrap_or(0);
    for info in snapshot.displays.iter().take(count) {
        let req = RemoveDisplayRequest::by_guid(info.monitor_guid);
        let _ = runtime.engine.remove_display(FileKey(0), &req, &platform);
    }

    runtime.adapter.store(ptr::null_mut(), Ordering::Release);
    runtime
        .etw
        .log_adapter_event(b"ReleaseHardware", STATUS_SUCCESS, count as u32);
    drop(runtime);
}

/// Registers the UMDF 2 WDF driver and device-add callback.
///
/// # Safety
///
/// Must only be invoked by `WUDFHost.exe` via `DriverEntry`.
// SAFETY: Invoked by `WUDFHost.exe` with valid `driver` and `registry_path` pointers.
pub unsafe fn wdf_driver_entry(
    driver: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    let mut config = WDF_DRIVER_CONFIG {
        Size: size_of::<WDF_DRIVER_CONFIG>() as u32,
        EvtDriverDeviceAdd: Some(evt_driver_device_add),
        EvtDriverUnload: Some(evt_driver_unload),
        DriverInitFlags: 0,
        DriverPoolTag: u32::from_le_bytes(*b"ZVDD"),
    };

    let mut wdf_driver: WDFDRIVER = ptr::null_mut();
    // SAFETY: `driver`, `registry_path`, `config`, and `wdf_driver` satisfy `WdfDriverCreate`.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver,
            registry_path,
            ptr::null_mut(),
            &mut config,
            &mut wdf_driver,
        )
    }
}

// SAFETY: Registered as `EVT_WDF_DRIVER_UNLOAD`; `driver` is the valid framework driver object.
unsafe extern "C" fn evt_driver_unload(_driver: WDFDRIVER) {}

// SAFETY: Registered as `EVT_WDF_OBJECT_CONTEXT_CLEANUP` for `DeviceContext`.
unsafe extern "C" fn evt_device_context_cleanup(object: WDFOBJECT) {
    let device: WDFDEVICE = object.cast();
    teardown_device_runtime(device, false);
    // SAFETY: `device` carries `DeviceContext` initialized via `ptr::write` in `evt_driver_device_add`.
    let ctx = unsafe { get_device_context(device) };
    if !ctx.is_null() {
        // SAFETY: Drop `DeviceContext` fields in place exactly once before WDF frees the context.
        unsafe {
            ptr::drop_in_place(&raw mut (*ctx).runtime);
            ptr::drop_in_place(&raw mut (*ctx).lease_timer);
        }
    }
}

// SAFETY: Registered as `EVT_WDF_OBJECT_CONTEXT_CLEANUP` for `FileContext`.
unsafe extern "C" fn evt_file_context_cleanup(_object: WDFOBJECT) {}

// SAFETY: Registered as passive-level `EVT_WDF_TIMER` with `ParentObject = WDFDEVICE`.
unsafe extern "C" fn evt_lease_reap_timer(timer: WDFTIMER) {
    // SAFETY: `timer` was created with `ParentObject = device`.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfTimerGetParentObject, timer) };
    if let Some(runtime) = get_device_runtime(device.cast()) {
        let platform = WdfIddPlatform::new(&runtime);
        if let Ok(reaped) = runtime.engine.reap_expired_leases(&platform) {
            if reaped > 0 {
                runtime
                    .etw
                    .log_counter_u64(b"LeaseExpiredReaped", b"reaped_count", reaped as u64);
            }
        }
    }
}

// SAFETY: Registered as `EVT_WDF_DRIVER_DEVICE_ADD`; `device_init` is valid for initialization.
unsafe extern "C" fn evt_driver_device_add(
    _driver: WDFDRIVER,
    mut device_init: wdk_sys::PWDFDEVICE_INIT,
) -> NTSTATUS {
    let mut pnp_callbacks = WDF_PNPPOWER_EVENT_CALLBACKS {
        Size: size_of::<WDF_PNPPOWER_EVENT_CALLBACKS>() as u32,
        EvtDeviceD0Entry: Some(evt_device_d0_entry),
        EvtDeviceD0EntryPostInterruptsEnabled: None,
        EvtDeviceD0Exit: Some(evt_device_d0_exit),
        EvtDeviceD0ExitPreInterruptsDisabled: None,
        EvtDevicePrepareHardware: Some(evt_device_prepare_hardware),
        EvtDeviceReleaseHardware: Some(evt_device_release_hardware),
        EvtDeviceSelfManagedIoCleanup: None,
        EvtDeviceSelfManagedIoFlush: None,
        EvtDeviceSelfManagedIoInit: None,
        EvtDeviceSelfManagedIoSuspend: None,
        EvtDeviceSelfManagedIoRestart: None,
        EvtDeviceSurpriseRemoval: None,
        EvtDeviceQueryRemove: None,
        EvtDeviceQueryStop: None,
        EvtDeviceUsageNotification: None,
        EvtDeviceRelationsQuery: None,
        EvtDeviceUsageNotificationEx: None,
    };
    // SAFETY: `device_init` and `pnp_callbacks` are valid before `WdfDeviceCreate`.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetPnpPowerEventCallbacks,
            device_init,
            &mut pnp_callbacks,
        );
    }

    let mut file_config = WDF_FILEOBJECT_CONFIG {
        Size: size_of::<WDF_FILEOBJECT_CONFIG>() as u32,
        EvtDeviceFileCreate: Some(evt_device_file_create),
        EvtFileClose: Some(evt_file_close),
        EvtFileCleanup: Some(evt_file_cleanup),
        AutoForwardCleanupClose: wdk_sys::_WDF_TRI_STATE::WdfUseDefault,
        FileObjectClass: wdk_sys::_WDF_FILEOBJECT_CLASS::WdfFileObjectWdfCannotUseFsContexts,
    };
    let mut file_attrs =
        init_object_attributes(&FILE_CONTEXT_TYPE_INFO, Some(evt_file_context_cleanup));
    // SAFETY: `device_init`, `file_config`, and `file_attrs` are valid before `WdfDeviceCreate`.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetFileObjectConfig,
            device_init,
            &mut file_config,
            &mut file_attrs,
        );
    }

    let callbacks = build_callbacks(
        Some(evt_iddcx_device_io_control),
        Some(evt_iddcx_adapter_ready),
    );
    // SAFETY: `device_init` and `callbacks` satisfy `zerovdd_iddcx_device_init_config`.
    let idd_cfg_status =
        unsafe { zerovdd_iddcx_device_init_config(device_init.cast::<c_void>(), &callbacks) };
    if idd_cfg_status < 0 {
        return idd_cfg_status;
    }

    let mut device_attrs =
        init_object_attributes(&DEVICE_CONTEXT_TYPE_INFO, Some(evt_device_context_cleanup));
    let mut device: WDFDEVICE = ptr::null_mut();
    // SAFETY: `device_init`, `device_attrs`, and `device` satisfy `WdfDeviceCreate`.
    let create_status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            &mut device_init,
            &mut device_attrs,
            &mut device,
        )
    };
    if create_status < 0 {
        return create_status;
    }

    // SAFETY: `device` was just created with `DEVICE_CONTEXT_TYPE_INFO`.
    let ctx = unsafe { get_device_context(device) };
    if !ctx.is_null() {
        let runtime = Arc::new(DeviceRuntime {
            device: AtomicPtr::new(device.cast()),
            adapter: AtomicPtr::new(ptr::null_mut()),
            iddcx_caps: AtomicU32::new(0),
            engine: VddEngine::new(),
            d3d: Arc::new(D3D11DeviceManager::new()),
            workers: SwapChainRegistry::new(),
            etw: Arc::new(EtwLogger::new()),
        });
        // SAFETY: `ctx` points to zeroed `DeviceContext` memory allocated by WDF; `ptr::write`
        // initializes `Mutex` and `AtomicPtr` without creating an uninitialized `&mut`.
        unsafe {
            ptr::write(
                &raw mut (*ctx).runtime,
                Mutex::new(Some(Arc::clone(&runtime))),
            );
            ptr::write(&raw mut (*ctx).lease_timer, AtomicPtr::new(ptr::null_mut()));
        }
        register_active_runtime(runtime);

        let mut timer_config = WDF_TIMER_CONFIG {
            Size: size_of::<WDF_TIMER_CONFIG>() as u32,
            EvtTimerFunc: Some(evt_lease_reap_timer),
            Period: LEASE_REAP_PERIOD_MS,
            AutomaticSerialization: 0,
            TolerableDelay: 250,
            __bindgen_padding_0: [0; 4],
            UseHighResolutionTimer: 0,
        };
        let mut timer_attrs = WDF_OBJECT_ATTRIBUTES {
            Size: size_of::<WDF_OBJECT_ATTRIBUTES>() as u32,
            EvtCleanupCallback: None,
            EvtDestroyCallback: None,
            ExecutionLevel: wdk_sys::_WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive,
            SynchronizationScope: wdk_sys::_WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeNone,
            ParentObject: device.cast(),
            ContextSizeOverride: 0,
            ContextTypeInfo: ptr::null(),
        };
        let mut timer: WDFTIMER = ptr::null_mut();
        // SAFETY: `timer_config`, `timer_attrs`, and `timer` satisfy `WdfTimerCreate` with `device` parent.
        let timer_status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfTimerCreate,
                &mut timer_config,
                &mut timer_attrs,
                &mut timer,
            )
        };
        if timer_status >= 0 && !timer.is_null() {
            // SAFETY: `ctx` is initialized and `timer` is parented to `device`.
            unsafe {
                (*ctx).lease_timer.store(timer.cast(), Ordering::Release);
            }
        }
    }

    let ref_string = control_reference_unicode_string();
    // SAFETY: `device`, `CONTROL_INTERFACE_GUID`, and `ref_string` are valid for interface creation.
    let iface_status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreateDeviceInterface,
            device,
            &CONTROL_INTERFACE_GUID,
            &ref_string,
        )
    };
    if iface_status < 0 {
        return iface_status;
    }

    // SAFETY: `device` is a created WDF device ready for IddCx binding.
    unsafe { zerovdd_iddcx_device_initialize(device.cast::<c_void>()) }
}

// SAFETY: Registered as `EVT_WDF_DEVICE_PREPARE_HARDWARE`; `device` is a valid `WDFDEVICE`.
unsafe extern "C" fn evt_device_prepare_hardware(
    device: WDFDEVICE,
    _resources_raw: WDFCMRESLIST,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    // SAFETY: `zerovdd_iddcx_probe_v110` reads only IddCx runtime stub globals.
    let caps = unsafe { zerovdd_iddcx_probe_v110() };
    let mut iddcx_version: u32 = 0;
    // SAFETY: `iddcx_version` is a valid stack `u32`.
    let _ver_status = unsafe { zerovdd_iddcx_get_version(&mut iddcx_version) };

    if let Some(runtime) = get_device_runtime(device.cast()) {
        runtime.iddcx_caps.store(caps, Ordering::Release);
        // SAFETY: Pure capability bitflag evaluation.
        let hdr_capable = unsafe { zerovdd_iddcx_supports_fp16(caps) != 0 };
        runtime.engine.set_hdr_capable(hdr_capable);
        runtime
            .etw
            .log_adapter_event(b"PrepareHardware", STATUS_SUCCESS, iddcx_version);
    }
    STATUS_SUCCESS
}

// SAFETY: Registered as `EVT_WDF_DEVICE_RELEASE_HARDWARE`; `device` is a valid `WDFDEVICE`.
unsafe extern "C" fn evt_device_release_hardware(
    device: WDFDEVICE,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    teardown_device_runtime(device, true);
    STATUS_SUCCESS
}

// SAFETY: Registered as `EVT_WDF_DEVICE_D0_ENTRY`; `device` is a valid `WDFDEVICE`.
unsafe extern "C" fn evt_device_d0_entry(
    device: WDFDEVICE,
    _previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    let Some(runtime) = get_device_runtime(device.cast()) else {
        return STATUS_SUCCESS;
    };
    let platform = WdfIddPlatform::new(&runtime);
    match runtime.engine.on_d0_entry(&platform) {
        Ok(initialized) => {
            // SAFETY: `device` carries `DeviceContext`.
            let ctx = unsafe { get_device_context(device) };
            if !ctx.is_null() {
                // SAFETY: `ctx` is initialized; start the passive periodic lease reaper timer.
                let timer_ptr = unsafe { (*ctx).lease_timer.load(Ordering::Acquire) };
                if !timer_ptr.is_null() {
                    // SAFETY: `timer_ptr` is a valid `WDFTIMER` created in `evt_driver_device_add`.
                    unsafe {
                        let _started = call_unsafe_wdf_function_binding!(
                            WdfTimerStart,
                            timer_ptr.cast(),
                            LEASE_REAP_DUE_TIME_100NS,
                        );
                    }
                }
            }
            runtime
                .etw
                .log_adapter_event(b"D0Entry", STATUS_SUCCESS, u32::from(initialized));
            STATUS_SUCCESS
        }
        Err(err) => {
            let status = match err {
                zerovdd_engine::EngineError::Platform(
                    zerovdd_engine::PlatformError::InitAdapterFailed { code },
                ) if code < 0 => code,
                _ => STATUS_UNSUCCESSFUL,
            };
            runtime.etw.log_adapter_event(b"D0Entry", status, 0);
            status
        }
    }
}

// SAFETY: Registered as `EVT_WDF_DEVICE_D0_EXIT`; `device` is a valid `WDFDEVICE`.
unsafe extern "C" fn evt_device_d0_exit(
    device: WDFDEVICE,
    _target_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    stop_lease_timer(device, false);
    if let Some(runtime) = get_device_runtime(device.cast()) {
        let state = runtime.engine.on_d0_exit();
        runtime
            .etw
            .log_adapter_event(b"D0Exit", STATUS_SUCCESS, state as u32);
    }
    STATUS_SUCCESS
}

// SAFETY: Invoked by the trampoline when adapter initialization finishes for `device`.
unsafe extern "system" fn evt_iddcx_adapter_ready(
    device: WdfDevicePtr,
    adapter_init_status: NtStatus,
) {
    if let Some(runtime) = get_device_runtime(device) {
        runtime
            .engine
            .on_adapter_init_finished(adapter_init_status >= 0);
        runtime
            .etw
            .log_adapter_event(b"AdapterInitFinished", adapter_init_status, 0);
    }
}
