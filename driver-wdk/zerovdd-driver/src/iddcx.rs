//! `IddPlatform` implementation and IddCx 1.10 callback handlers wired into `VddEngine`.

use core::ptr;
use core::sync::atomic::Ordering;

use wdk_sys::windows::GetTickCount64;
use zerovdd_engine::{
    AdapterLuid, ConnectorSlot, IddPlatform, MonitorArrivalResult, MonitorHandle, MonitorQueryKey,
    PlatformError, SwapchainAssignAction, MAX_MONITORS,
};
use zerovdd_iddcx_sys::{
    zerovdd_iddcx_adapter_init_async, zerovdd_iddcx_adapter_set_render_adapter,
    zerovdd_iddcx_monitor_arrival, zerovdd_iddcx_monitor_create, zerovdd_iddcx_monitor_departure,
    zerovdd_iddcx_monitor_update_modes, zerovdd_iddcx_probe_v110, zerovdd_iddcx_supports_fp16,
    IddCxAdapterPtr, IddCxMonitorPtr, IddCxSwapChainPtr, NtStatus, RawHandle, WdfDevicePtr,
    ZeroVddCommitPath, ZeroVddHdr10Metadata, ZeroVddIddCxCallbacks, ZeroVddTargetInfo,
    ZeroVddVideoSignal, BPC_10, BPC_12, BPC_8, IDDCX_CAP_MONITOR_UPDATE_MODES2, NO_PREFERRED_MODE,
    PATH_FLAG_ACTIVE, TARGET_CAPS_HIGH_COLOR_SPACE, TARGET_CAPS_NONE, TARGET_CAPS_WIDE_COLOR_SPACE,
};
use zerovdd_modes::{
    compute_cvt_rb2_monitor_signal, BitDepthMask, PlannedMode, RationalHz, VideoSignalInfo,
};

use crate::swapchain::SwapChainWorker;
use crate::wdf::{
    find_runtime_for_edid_lookup, get_device_runtime, DeviceRuntime, STATUS_BUFFER_TOO_SMALL,
    STATUS_CANCELLED, STATUS_GRAPHICS_INDIRECT_DISPLAY_ABANDON_SWAPCHAIN,
    STATUS_INSUFFICIENT_RESOURCES, STATUS_INVALID_PARAMETER, STATUS_NOT_FOUND,
    STATUS_NO_DATA_DETECTED, STATUS_SUCCESS,
};

/// Platform adapter bridging `zerovdd_engine::IddPlatform` to UMDF 2 / IddCx 1.10 DDIs.
pub struct WdfIddPlatform<'a> {
    runtime: &'a DeviceRuntime,
}

impl<'a> WdfIddPlatform<'a> {
    /// Creates a lightweight platform adapter borrowing `runtime`.
    #[must_use]
    pub const fn new(runtime: &'a DeviceRuntime) -> Self {
        Self { runtime }
    }
}

impl IddPlatform for WdfIddPlatform<'_> {
    fn init_adapter(&self) -> Result<(), PlatformError> {
        let caps = self.runtime.iddcx_caps.load(Ordering::Acquire);
        // SAFETY: `zerovdd_iddcx_supports_fp16` inspects only pure capability bitflags.
        let fp16 = unsafe { zerovdd_iddcx_supports_fp16(caps) };
        let device = self.runtime.device.load(Ordering::Acquire);
        let mut adapter: IddCxAdapterPtr = ptr::null_mut();
        // SAFETY: `device` is initialized with `IddCxDeviceInitialize`.
        let status = unsafe {
            zerovdd_iddcx_adapter_init_async(device, MAX_MONITORS as u32, fp16, &mut adapter)
        };
        self.runtime
            .etw
            .log_adapter_event(b"AdapterInitAsync", status, u32::from(fp16));
        if status < 0 {
            return Err(PlatformError::InitAdapterFailed { code: status });
        }
        self.runtime.adapter.store(adapter, Ordering::Release);
        Ok(())
    }

    fn create_monitor(
        &self,
        slot: ConnectorSlot,
        monitor_guid: [u8; 16],
        edid: &[u8],
    ) -> Result<MonitorHandle, PlatformError> {
        let adapter = self.runtime.adapter.load(Ordering::Acquire);
        if adapter.is_null() {
            return Err(PlatformError::CreateMonitorFailed {
                slot: slot.0,
                code: STATUS_INVALID_PARAMETER,
            });
        }

        let device = self.runtime.device.load(Ordering::Acquire);
        let mut monitor: IddCxMonitorPtr = ptr::null_mut();
        let edid_len = u32::try_from(edid.len()).unwrap_or(u32::MAX);
        // SAFETY: `device`, `adapter`, `monitor_guid`, and `edid` remain valid across call.
        let status = unsafe {
            zerovdd_iddcx_monitor_create(
                device,
                adapter,
                slot.0,
                monitor_guid.as_ptr(),
                edid.as_ptr(),
                edid_len,
                &mut monitor,
            )
        };
        self.runtime
            .etw
            .log_monitor_event(b"MonitorCreate", slot.0, status);
        if status < 0 || monitor.is_null() {
            return Err(PlatformError::CreateMonitorFailed {
                slot: slot.0,
                code: status,
            });
        }
        Ok(MonitorHandle(monitor as usize as u64))
    }

    fn arrive_monitor(
        &self,
        handle: MonitorHandle,
        slot: ConnectorSlot,
        _monitor_guid: [u8; 16],
    ) -> Result<MonitorArrivalResult, PlatformError> {
        let monitor = handle.0 as usize as IddCxMonitorPtr;
        if monitor.is_null() {
            return Err(PlatformError::ArriveMonitorFailed {
                slot: slot.0,
                code: STATUS_INVALID_PARAMETER,
            });
        }
        let mut luid_low: u32 = 0;
        let mut luid_high: i32 = 0;
        let mut target_id: u32 = 0;
        // SAFETY: `monitor` is a valid `IDDCX_MONITOR` handle returned by `IddCxMonitorCreate`.
        let status = unsafe {
            zerovdd_iddcx_monitor_arrival(monitor, &mut luid_low, &mut luid_high, &mut target_id)
        };
        self.runtime
            .etw
            .log_monitor_event(b"MonitorArrival", target_id, status);
        if status < 0 {
            return Err(PlatformError::ArriveMonitorFailed {
                slot: slot.0,
                code: status,
            });
        }
        Ok(MonitorArrivalResult {
            adapter_luid: AdapterLuid::new(luid_low, luid_high),
            target_id,
        })
    }

    fn depart_monitor(&self, handle: MonitorHandle) -> Result<(), PlatformError> {
        let monitor = handle.0 as usize as IddCxMonitorPtr;
        if monitor.is_null() {
            return Err(PlatformError::DepartMonitorFailed {
                handle: handle.0,
                code: STATUS_INVALID_PARAMETER,
            });
        }
        // WHY: Mark departing and drain both in-flight assignments and active workers before
        // `IddCxMonitorDeparture` so `WdfObjectDelete(hSwapChain)` and D3D11 teardown finish
        // while `IDDCX_MONITOR` is valid, preventing concurrent or zombie worker creation.
        self.runtime.workers.begin_depart_and_drain(monitor);
        // SAFETY: `monitor` is a live `IDDCX_MONITOR` handle owned by this adapter.
        let status = unsafe { zerovdd_iddcx_monitor_departure(monitor) };
        self.runtime.workers.finish_depart(monitor);
        self.runtime
            .etw
            .log_monitor_event(b"MonitorDeparture", handle.0 as u32, status);
        if status < 0 {
            return Err(PlatformError::DepartMonitorFailed {
                handle: handle.0,
                code: status,
            });
        }
        Ok(())
    }

    fn update_monitor_modes(
        &self,
        handle: MonitorHandle,
        planned_modes: &[PlannedMode],
    ) -> Result<(), PlatformError> {
        let monitor = handle.0 as usize as IddCxMonitorPtr;
        let mode_count = u32::try_from(planned_modes.len()).unwrap_or(0);
        if monitor.is_null() || mode_count == 0 {
            return Err(PlatformError::UpdateMonitorModesFailed {
                handle: handle.0,
                code: STATUS_INVALID_PARAMETER,
            });
        }

        let mut signals = alloc::vec::Vec::with_capacity(planned_modes.len());
        for mode in planned_modes {
            signals.push(to_c_signal(mode.target_signal, mode.bit_depths));
        }

        let caps = self.runtime.iddcx_caps.load(Ordering::Acquire);
        // SAFETY: `zerovdd_iddcx_supports_fp16` inspects only pure capability bitflags.
        let fp16 = unsafe { zerovdd_iddcx_supports_fp16(caps) != 0 };
        let use_v2 = u8::from((caps & IDDCX_CAP_MONITOR_UPDATE_MODES2) != 0 && fp16);

        // SAFETY: `monitor` is an active `IDDCX_MONITOR` handle and `signals` holds `mode_count` entries.
        let status = unsafe {
            zerovdd_iddcx_monitor_update_modes(monitor, use_v2, signals.as_ptr(), mode_count)
        };
        self.runtime
            .etw
            .log_monitor_event(b"MonitorUpdateModes", handle.0 as u32, status);
        if status < 0 {
            return Err(PlatformError::UpdateMonitorModesFailed {
                handle: handle.0,
                code: status,
            });
        }
        Ok(())
    }

    fn set_render_adapter(&self, luid: AdapterLuid) -> Result<(), PlatformError> {
        let adapter = self.runtime.adapter.load(Ordering::Acquire);
        if adapter.is_null() {
            return Err(PlatformError::SetRenderAdapterFailed {
                low: luid.low,
                high: luid.high,
                code: STATUS_INVALID_PARAMETER,
            });
        }
        // SAFETY: `adapter` is a valid `IDDCX_ADAPTER` handle.
        let status =
            unsafe { zerovdd_iddcx_adapter_set_render_adapter(adapter, luid.low, luid.high) };
        self.runtime
            .etw
            .log_gpu_switch(b"SetRenderAdapter", luid.low, luid.high, status);
        if status < 0 {
            return Err(PlatformError::SetRenderAdapterFailed {
                low: luid.low,
                high: luid.high,
                code: status,
            });
        }
        Ok(())
    }

    fn now_monotonic_ms(&self) -> u64 {
        // SAFETY: `GetTickCount64` requires no preconditions and never fails.
        unsafe { GetTickCount64() }
    }
}

/// Builds the complete [`ZeroVddIddCxCallbacks`] registration table for `IddCxDeviceInitConfig`.
#[must_use]
pub fn build_callbacks(
    evt_io_control: zerovdd_iddcx_sys::EvtIddCxDeviceIoControl,
    evt_adapter_ready: zerovdd_iddcx_sys::EvtAdapterReady,
) -> ZeroVddIddCxCallbacks {
    ZeroVddIddCxCallbacks {
        evt_io_control,
        evt_adapter_ready,
        evt_query_target_info: Some(evt_iddcx_query_target_info),
        evt_parse_monitor_description: Some(evt_iddcx_parse_monitor_description),
        evt_get_default_description_modes: Some(evt_iddcx_get_default_description_modes),
        evt_query_target_modes: Some(evt_iddcx_query_target_modes),
        evt_commit_modes: Some(evt_iddcx_commit_modes),
        evt_set_default_hdr_metadata: Some(evt_iddcx_set_default_hdr_metadata),
        evt_set_gamma_ramp: Some(evt_iddcx_set_gamma_ramp),
        evt_assign_swapchain: Some(evt_iddcx_assign_swapchain),
        evt_unassign_swapchain: Some(evt_iddcx_unassign_swapchain),
    }
}

fn bit_depth_mask_to_bpc_rgb(mask: BitDepthMask) -> u32 {
    let mut bpc = 0;
    if mask.contains(BitDepthMask::BITS_8) {
        bpc |= BPC_8;
    }
    if mask.contains(BitDepthMask::BITS_10) {
        bpc |= BPC_10;
    }
    if mask.contains(BitDepthMask::BITS_12) {
        bpc |= BPC_12;
    }
    if bpc == 0 {
        BPC_8
    } else {
        bpc
    }
}

fn to_c_signal(sig: VideoSignalInfo, bit_depths: BitDepthMask) -> ZeroVddVideoSignal {
    ZeroVddVideoSignal {
        pixel_rate: sig.pixel_rate,
        active_width: sig.active_width,
        active_height: sig.active_height,
        total_width: sig.total_width,
        total_height: sig.total_height,
        hsync_freq_num: sig.hsync_freq_num,
        hsync_freq_den: sig.hsync_freq_den,
        vsync_freq_num: sig.vsync_freq_num,
        vsync_freq_den: sig.vsync_freq_den,
        vsync_freq_divider: sig.vsync_freq_divider,
        bits_per_component_rgb: bit_depth_mask_to_bpc_rgb(bit_depths),
    }
}

// SAFETY: Registered as `EvtQueryTargetInfo`; `out_info` is a valid non-null pointer from C.
unsafe extern "system" fn evt_iddcx_query_target_info(
    device: WdfDevicePtr,
    _connector_index: u32,
    out_info: *mut ZeroVddTargetInfo,
) -> NtStatus {
    if out_info.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let fp16 = if let Some(runtime) = get_device_runtime(device) {
        let caps = runtime.iddcx_caps.load(Ordering::Acquire);
        // SAFETY: Pure bitflag evaluation.
        unsafe { zerovdd_iddcx_supports_fp16(caps) != 0 }
    } else {
        // SAFETY: `IddCxAdapterInitAsync` may invoke `EvtIddCxAdapterQueryTargetInfo` before
        // `ctx->Device` is populated; probe runtime IddCx 1.10 capabilities directly.
        unsafe { zerovdd_iddcx_supports_fp16(zerovdd_iddcx_probe_v110()) != 0 }
    };

    let info = if fp16 {
        ZeroVddTargetInfo {
            target_caps: TARGET_CAPS_WIDE_COLOR_SPACE | TARGET_CAPS_HIGH_COLOR_SPACE,
            dithering_rgb: BPC_8 | BPC_10,
        }
    } else {
        ZeroVddTargetInfo {
            target_caps: TARGET_CAPS_NONE,
            dithering_rgb: 0,
        }
    };
    // SAFETY: Caller guarantees `out_info` points to writable `ZeroVddTargetInfo`.
    unsafe {
        ptr::write(out_info, info);
    }
    STATUS_SUCCESS
}

// SAFETY: Registered as `EvtParseMonitorDescription`; `edid_data`, `out_modes`, `out_count`, and
// `out_preferred_idx` satisfy the trampoline contract.
unsafe extern "system" fn evt_iddcx_parse_monitor_description(
    edid_data: *const u8,
    edid_size: u32,
    max_modes: u32,
    out_modes: *mut ZeroVddVideoSignal,
    out_count: *mut u32,
    out_preferred_idx: *mut u32,
) -> NtStatus {
    if out_count.is_null() || out_preferred_idx.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: Output pointers are non-null locals provided by the trampoline.
    unsafe {
        ptr::write(out_count, 0);
        ptr::write(out_preferred_idx, NO_PREFERRED_MODE);
    }
    if edid_size == 0 || edid_data.is_null() {
        return STATUS_NO_DATA_DETECTED;
    }

    // SAFETY: `edid_data` is valid for `edid_size` bytes across this synchronous callback.
    let edid_slice = unsafe { core::slice::from_raw_parts(edid_data, edid_size as usize) };
    let Some(parsed) = find_runtime_for_edid_lookup(edid_slice) else {
        return STATUS_NOT_FOUND;
    };

    let total_u32 = u32::try_from(parsed.spec.planned_modes.len()).unwrap_or(0);
    if total_u32 == 0 {
        return STATUS_NO_DATA_DETECTED;
    }

    // SAFETY: `out_count` and `out_preferred_idx` are valid writable pointers.
    unsafe {
        ptr::write(out_count, total_u32);
        ptr::write(out_preferred_idx, 0);
    }

    if out_modes.is_null() {
        return if max_modes == 0 {
            STATUS_SUCCESS
        } else {
            STATUS_INVALID_PARAMETER
        };
    }
    if max_modes < total_u32 {
        return STATUS_BUFFER_TOO_SMALL;
    }

    // SAFETY: `out_modes` has capacity for at least `total_u32` elements (`max_modes >= total_u32`).
    let dst = unsafe { core::slice::from_raw_parts_mut(out_modes, total_u32 as usize) };
    for (slot, planned) in dst.iter_mut().zip(parsed.spec.planned_modes.iter()) {
        *slot = to_c_signal(planned.monitor_signal, planned.bit_depths);
    }
    STATUS_SUCCESS
}

// SAFETY: Registered as `EvtGetDefaultDescriptionModes`; pointer args are validated before write.
unsafe extern "system" fn evt_iddcx_get_default_description_modes(
    device: WdfDevicePtr,
    monitor: IddCxMonitorPtr,
    max_modes: u32,
    out_modes: *mut ZeroVddVideoSignal,
    out_count: *mut u32,
    out_preferred_idx: *mut u32,
) -> NtStatus {
    if out_count.is_null() || out_preferred_idx.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    if let Some(runtime) = get_device_runtime(device) {
        let key = MonitorQueryKey::MonitorHandle(MonitorHandle(monitor as usize as u64));
        if let Ok(parsed) = runtime.engine.on_parse_monitor_description(key) {
            let total_u32 = u32::try_from(parsed.spec.planned_modes.len()).unwrap_or(0);
            if total_u32 > 0 {
                // SAFETY: Output pointers are non-null locals provided by the trampoline.
                unsafe {
                    ptr::write(out_count, total_u32);
                    ptr::write(out_preferred_idx, 0);
                }
                if out_modes.is_null() {
                    return if max_modes == 0 {
                        STATUS_SUCCESS
                    } else {
                        STATUS_INVALID_PARAMETER
                    };
                }
                if max_modes < total_u32 {
                    return STATUS_BUFFER_TOO_SMALL;
                }
                // SAFETY: `out_modes` has capacity `max_modes >= total_u32`.
                let dst = unsafe { core::slice::from_raw_parts_mut(out_modes, total_u32 as usize) };
                for (slot, planned) in dst.iter_mut().zip(parsed.spec.planned_modes.iter()) {
                    *slot = to_c_signal(planned.monitor_signal, planned.bit_depths);
                }
                return STATUS_SUCCESS;
            }
        }
    }

    let Ok(hz60) = RationalHz::from_hz(60) else {
        return STATUS_NO_DATA_DETECTED;
    };
    let Ok(fallback_sig) = compute_cvt_rb2_monitor_signal(1920, 1080, hz60) else {
        return STATUS_NO_DATA_DETECTED;
    };
    // SAFETY: `out_count` and `out_preferred_idx` are valid writable pointers.
    unsafe {
        ptr::write(out_count, 1);
        ptr::write(out_preferred_idx, 0);
    }
    if out_modes.is_null() {
        return if max_modes == 0 {
            STATUS_SUCCESS
        } else {
            STATUS_INVALID_PARAMETER
        };
    }
    if max_modes < 1 {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: `out_modes` has capacity for at least 1 element (`max_modes >= 1`).
    unsafe {
        ptr::write(
            out_modes,
            to_c_signal(fallback_sig, BitDepthMask::SDR_DEFAULT),
        );
    }
    STATUS_SUCCESS
}

// SAFETY: Registered as `EvtQueryTargetModes`; pointer args are validated before write.
unsafe extern "system" fn evt_iddcx_query_target_modes(
    device: WdfDevicePtr,
    monitor: IddCxMonitorPtr,
    max_modes: u32,
    out_modes: *mut ZeroVddVideoSignal,
    out_count: *mut u32,
) -> NtStatus {
    if out_count.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: `out_count` is a valid writable pointer.
    unsafe {
        ptr::write(out_count, 0);
    }
    let Some(runtime) = get_device_runtime(device) else {
        return STATUS_NOT_FOUND;
    };
    let key = MonitorQueryKey::MonitorHandle(MonitorHandle(monitor as usize as u64));
    let Ok(res) = runtime.engine.on_query_target_modes(key) else {
        return STATUS_NOT_FOUND;
    };

    let total_u32 = u32::try_from(res.planned_modes.len()).unwrap_or(0);
    if total_u32 == 0 {
        return STATUS_NO_DATA_DETECTED;
    }
    // SAFETY: `out_count` is a valid writable pointer.
    unsafe {
        ptr::write(out_count, total_u32);
    }

    if out_modes.is_null() {
        return if max_modes == 0 {
            STATUS_SUCCESS
        } else {
            STATUS_INVALID_PARAMETER
        };
    }
    if max_modes < total_u32 {
        return STATUS_BUFFER_TOO_SMALL;
    }

    // SAFETY: `out_modes` has capacity `max_modes >= total_u32`.
    let dst = unsafe { core::slice::from_raw_parts_mut(out_modes, total_u32 as usize) };
    for (slot, planned) in dst.iter_mut().zip(res.planned_modes.iter()) {
        *slot = to_c_signal(planned.target_signal, planned.bit_depths);
    }
    STATUS_SUCCESS
}

// SAFETY: Registered as `EvtCommitModes`; `paths` is valid for `path_count` items when non-zero.
unsafe extern "system" fn evt_iddcx_commit_modes(
    device: WdfDevicePtr,
    path_count: u32,
    paths: *const ZeroVddCommitPath,
) -> NtStatus {
    if path_count > 0 && paths.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let Some(runtime) = get_device_runtime(device) else {
        return STATUS_SUCCESS;
    };
    if path_count == 0 {
        return STATUS_SUCCESS;
    }
    // SAFETY: `paths` is non-null and valid for `path_count` entries across this call.
    let path_slice = unsafe { core::slice::from_raw_parts(paths, path_count as usize) };
    for path in path_slice {
        if (path.flags & PATH_FLAG_ACTIVE) != 0 {
            runtime.etw.log_mode_commit(
                path.active_width,
                path.active_height,
                path.color_space,
                path.bits_per_component_rgb,
            );
        }
    }
    STATUS_SUCCESS
}

// SAFETY: Registered as `EvtSetDefaultHdrMetadata`; `hdr10` is validated before dereference.
unsafe extern "system" fn evt_iddcx_set_default_hdr_metadata(
    device: WdfDevicePtr,
    monitor: IddCxMonitorPtr,
    metadata_type: u32,
    hdr10: *const ZeroVddHdr10Metadata,
) -> NtStatus {
    let Some(runtime) = get_device_runtime(device) else {
        return STATUS_SUCCESS;
    };
    let max_nits = if !hdr10.is_null() {
        // SAFETY: `hdr10` is non-null and points to a valid `ZeroVddHdr10Metadata`.
        u32::from(unsafe { (*hdr10).max_mastering_luminance })
    } else {
        0
    };
    runtime
        .etw
        .log_adapter_event(b"SetDefaultHdrMeta", metadata_type as i32, max_nits);
    let _ = monitor;
    STATUS_SUCCESS
}

// SAFETY: Registered as `EvtSetGammaRamp`; invoked with valid `device` and `monitor`.
unsafe extern "system" fn evt_iddcx_set_gamma_ramp(
    device: WdfDevicePtr,
    monitor: IddCxMonitorPtr,
    ramp_type: u32,
    ramp_size_bytes: u32,
) -> NtStatus {
    if let Some(runtime) = get_device_runtime(device) {
        runtime
            .etw
            .log_adapter_event(b"SetGammaRamp", ramp_type as i32, ramp_size_bytes);
    }
    let _ = monitor;
    STATUS_SUCCESS
}

// SAFETY: Registered as `EvtAssignSwapChain`; `monitor`, `swapchain`, and `next_surface_available_event`
// are valid handles provided by IddCx.
unsafe extern "system" fn evt_iddcx_assign_swapchain(
    device: WdfDevicePtr,
    monitor: IddCxMonitorPtr,
    swapchain: IddCxSwapChainPtr,
    next_surface_available_event: RawHandle,
    render_luid_low: u32,
    render_luid_high: i32,
) -> NtStatus {
    let Some(runtime) = get_device_runtime(device) else {
        return STATUS_INVALID_PARAMETER;
    };
    let current_luid = AdapterLuid::new(render_luid_low, render_luid_high);

    if matches!(
        runtime.engine.on_assign_swapchain(current_luid),
        SwapchainAssignAction::AbandonSwapChain
    ) {
        runtime.etw.log_gpu_switch(
            b"AssignSwapChainAbandon",
            current_luid.low,
            current_luid.high,
            STATUS_GRAPHICS_INDIRECT_DISPLAY_ABANDON_SWAPCHAIN,
        );
        return STATUS_GRAPHICS_INDIRECT_DISPLAY_ABANDON_SWAPCHAIN;
    }

    let Some(assign_guard) = runtime.workers.try_begin_assign(monitor) else {
        return STATUS_CANCELLED;
    };

    let caps = runtime.iddcx_caps.load(Ordering::Acquire);
    // SAFETY: Pure bitflag evaluation.
    let use_v2 = unsafe { zerovdd_iddcx_supports_fp16(caps) != 0 };

    let Some(worker) = SwapChainWorker::start(
        monitor,
        swapchain,
        next_surface_available_event,
        current_luid,
        use_v2,
        alloc::sync::Arc::clone(&runtime.d3d),
        alloc::sync::Arc::clone(&runtime.etw),
    ) else {
        runtime.etw.log_monitor_event(
            b"SwapChainStartFailed",
            monitor as usize as u32,
            STATUS_INSUFFICIENT_RESOURCES,
        );
        return STATUS_INSUFFICIENT_RESOURCES;
    };

    if !assign_guard.commit(worker) {
        return STATUS_SUCCESS;
    }
    runtime.etw.log_gpu_switch(
        b"AssignSwapChain",
        current_luid.low,
        current_luid.high,
        STATUS_SUCCESS,
    );
    STATUS_SUCCESS
}

// SAFETY: Registered as `EvtUnassignSwapChain`; `monitor` is a valid `IDDCX_MONITOR`.
unsafe extern "system" fn evt_iddcx_unassign_swapchain(
    device: WdfDevicePtr,
    monitor: IddCxMonitorPtr,
) -> NtStatus {
    if let Some(runtime) = get_device_runtime(device) {
        runtime.workers.remove_and_stop(monitor);
        runtime.etw.log_monitor_event(
            b"UnassignSwapChain",
            monitor as usize as u32,
            STATUS_SUCCESS,
        );
    }
    STATUS_SUCCESS
}
