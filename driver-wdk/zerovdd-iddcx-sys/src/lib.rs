//! C-ABI trampoline and FFI bindings to UMDF 2.33 / IddCx 1.10.

#![no_std]

use core::ffi::c_void;

/// Opaque WDF device initialization pointer passed between UMDF callbacks and IddCx.
pub type WdfDeviceInitPtr = *mut c_void;

/// Opaque WDF device handle passed between UMDF callbacks and IddCx.
pub type WdfDevicePtr = *mut c_void;

/// Opaque WDF request handle passed to the IddCx IOCTL callback.
pub type WdfRequestPtr = *mut c_void;

/// Opaque IddCx adapter handle returned by [`zerovdd_iddcx_adapter_init_async`].
pub type IddCxAdapterPtr = *mut c_void;

/// Opaque IddCx monitor handle returned by [`zerovdd_iddcx_monitor_create`].
pub type IddCxMonitorPtr = *mut c_void;

/// Opaque IddCx swapchain handle passed to `EvtIddCxMonitorAssignSwapChain`.
pub type IddCxSwapChainPtr = *mut c_void;

/// Opaque Win32 event/object handle passed across the IddCx C ABI.
pub type RawHandle = *mut c_void;

/// Windows `NTSTATUS` return code used across WDF and IddCx DDIs.
pub type NtStatus = i32;

/// Windows `HRESULT` return code used across DXGI, D3D11, and IddCx swapchain DDIs.
pub type HResult = i32;

/// `IDD_IS_FUNCTION_AVAILABLE(IddCxAdapterSetRenderAdapter)` capability bit.
pub const IDDCX_CAP_SET_RENDER_ADAPTER: u32 = 1 << 0;
/// `IDD_IS_FUNCTION_AVAILABLE(IddCxAdapterDisplayConfigUpdate2)` capability bit.
pub const IDDCX_CAP_DISPLAY_CONFIG_UPDATE2: u32 = 1 << 1;
/// `IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorUpdateModes2)` capability bit.
pub const IDDCX_CAP_MONITOR_UPDATE_MODES2: u32 = 1 << 2;
/// `IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorQueryHardwareCursor3)` capability bit.
pub const IDDCX_CAP_QUERY_HW_CURSOR3: u32 = 1 << 3;
/// `IDD_IS_FIELD_AVAILABLE(IDDCX_ADAPTER_CAPS, Flags)` capability bit.
pub const IDDCX_CAP_ADAPTER_CAPS_FLAGS: u32 = 1 << 4;
/// `IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxParseMonitorDescription2)` bit.
pub const IDDCX_CAP_PARSE_DESC2: u32 = 1 << 5;
/// `IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorQueryTargetModes2)` bit.
pub const IDDCX_CAP_QUERY_MODES2: u32 = 1 << 6;
/// `IDD_IS_FUNCTION_AVAILABLE(IddCxSwapChainReleaseAndAcquireBuffer2)` capability bit.
pub const IDDCX_CAP_RELEASE_ACQUIRE2: u32 = 1 << 7;
/// `IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorQueryHardwareCursor2)` capability bit.
pub const IDDCX_CAP_QUERY_HW_CURSOR2: u32 = 1 << 8;
/// `IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterQueryTargetInfo)` bit.
pub const IDDCX_CAP_QUERY_TARGET_INFO: u32 = 1 << 9;
/// `IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterCommitModes2)` bit.
pub const IDDCX_CAP_COMMIT_MODES2: u32 = 1 << 10;
/// `IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorSetDefaultHdrMetaData)` bit.
pub const IDDCX_CAP_SET_DEFAULT_HDR_META: u32 = 1 << 11;

/// Wire bit-depth mask: no bits per component set.
pub const BPC_NONE: u32 = 0;
/// Wire bit-depth mask: 8 bits per component (`IDDCX_BITS_PER_COMPONENT_8`).
pub const BPC_8: u32 = 0x02;
/// Wire bit-depth mask: 10 bits per component (`IDDCX_BITS_PER_COMPONENT_10`).
pub const BPC_10: u32 = 0x04;
/// Wire bit-depth mask: 12 bits per component (`IDDCX_BITS_PER_COMPONENT_12`).
pub const BPC_12: u32 = 0x08;

/// Target capability bit: none.
pub const TARGET_CAPS_NONE: u32 = 0;
/// Target capability bit: `IDDCX_TARGET_CAPS_WIDE_COLOR_SPACE`.
pub const TARGET_CAPS_WIDE_COLOR_SPACE: u32 = 0x1;
/// Target capability bit: `IDDCX_TARGET_CAPS_HIGH_COLOR_SPACE`.
pub const TARGET_CAPS_HIGH_COLOR_SPACE: u32 = 0x2;

/// Committed path flag: `IDDCX_PATH_FLAGS_CHANGED`.
pub const PATH_FLAG_CHANGED: u32 = 0x1;
/// Committed path flag: `IDDCX_PATH_FLAGS_ACTIVE`.
pub const PATH_FLAG_ACTIVE: u32 = 0x2;

/// Sentinel returned by `ParseMonitorDescription` when no preferred mode exists.
pub const NO_PREFERRED_MODE: u32 = 0xFFFF_FFFF;

/// C-ABI video signal descriptor shared between Rust and the IddCx trampoline.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ZeroVddVideoSignal {
    /// Pixel clock rate in Hz.
    pub pixel_rate: u64,
    /// Active horizontal pixels.
    pub active_width: u32,
    /// Active vertical lines.
    pub active_height: u32,
    /// Total horizontal pixels including blanking.
    pub total_width: u32,
    /// Total vertical lines including VBI.
    pub total_height: u32,
    /// Horizontal sync frequency numerator.
    pub hsync_freq_num: u32,
    /// Horizontal sync frequency denominator.
    pub hsync_freq_den: u32,
    /// Vertical sync frequency numerator.
    pub vsync_freq_num: u32,
    /// Vertical sync frequency denominator.
    pub vsync_freq_den: u32,
    /// Vertical sync frequency divider (`0` for monitor modes, `1` for target modes).
    pub vsync_freq_divider: u32,
    /// RGB wire bits-per-component bitmask (`BPC_*`).
    pub bits_per_component_rgb: u32,
}

/// C-ABI target capabilities output filled by `EvtIddCxAdapterQueryTargetInfo`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ZeroVddTargetInfo {
    /// Bitmask of `TARGET_CAPS_*` flags.
    pub target_caps: u32,
    /// RGB dithering support bitmask (`BPC_*`).
    pub dithering_rgb: u32,
}

/// C-ABI committed mode path forwarded from `EvtIddCxAdapterCommitModes[2]`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZeroVddCommitPath {
    /// Target `IDDCX_MONITOR` handle.
    pub monitor: IddCxMonitorPtr,
    /// `PATH_FLAG_*` bitmask.
    pub flags: u32,
    /// Active width in pixels.
    pub active_width: u32,
    /// Active height in lines.
    pub active_height: u32,
    /// Committed vertical refresh numerator.
    pub vsync_num: u32,
    /// Committed vertical refresh denominator.
    pub vsync_den: u32,
    /// Committed `IDDCX_COLOR_SPACE` enum value.
    pub color_space: u32,
    /// Committed RGB bits-per-component bitmask.
    pub bits_per_component_rgb: u32,
}

/// C-ABI HDR10 static mastering metadata forwarded from `EvtIddCxMonitorSetDefaultHdrMetaData`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ZeroVddHdr10Metadata {
    /// CIE 1931 red primary normalized to 50,000.
    pub red_primary: [u16; 2],
    /// CIE 1931 green primary normalized to 50,000.
    pub green_primary: [u16; 2],
    /// CIE 1931 blue primary normalized to 50,000.
    pub blue_primary: [u16; 2],
    /// CIE 1931 white point normalized to 50,000.
    pub white_point: [u16; 2],
    /// Maximum mastering luminance in nits.
    pub max_mastering_luminance: u16,
    /// Minimum mastering luminance in 0.0001-nit units.
    pub min_mastering_luminance: u16,
    /// Maximum content light level in nits.
    pub max_content_light_level: u16,
    /// Maximum frame-average light level in nits.
    pub max_frame_average_light_level: u16,
}

/// C-ABI hardware cursor state returned by `zerovdd_iddcx_monitor_query_hw_cursor`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ZeroVddCursorInfo {
    /// Non-zero when the hardware cursor is visible.
    pub is_visible: u32,
    /// Screen X coordinate of cursor top-left pixel.
    pub x: i32,
    /// Screen Y coordinate of cursor top-left pixel.
    pub y: i32,
    /// Non-zero when a new cursor shape was copied into the caller buffer.
    pub is_shape_updated: u32,
    /// Monotonic cursor shape identifier.
    pub shape_id: u32,
    /// `IDDCX_CURSOR_SHAPE_TYPE` value.
    pub cursor_type: u32,
    /// Cursor image width in pixels.
    pub width: u32,
    /// Cursor image height in pixels.
    pub height: u32,
    /// Cursor row pitch in bytes.
    pub pitch: u32,
    /// Cursor hotspot X coordinate.
    pub x_hot: u32,
    /// Cursor hotspot Y coordinate.
    pub y_hot: u32,
    /// Non-zero when `x`, `y`, and `position_id` are valid.
    pub position_valid: u32,
    /// Monotonic cursor position identifier.
    pub position_id: u32,
    /// SDR white level in nits for HDR cursor compositing.
    pub sdr_white_level: u32,
}

/// C-ABI acquired frame metadata returned by `zerovdd_iddcx_swapchain_release_and_acquire_buffer`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ZeroVddFrameInfo {
    /// OS presentation frame sequence number.
    pub presentation_frame_number: u32,
    /// Number of dirty rectangles in the acquired frame.
    pub dirty_rect_count: u32,
    /// Number of move regions in the acquired frame.
    pub move_region_count: u32,
    /// `DXGI_COLOR_SPACE_TYPE` value when acquired via v2.
    pub surface_color_space: u32,
    /// SDR content white level in nits when acquired via v2.
    pub sdr_white_level: u32,
    /// Presentation QPC timestamp.
    pub present_display_qpc_time: u64,
}

/// Callback invoked by IddCx when a user-mode IOCTL reaches the adapter device.
pub type EvtIddCxDeviceIoControl = Option<
    // SAFETY: Invoked by IddCx with valid `WDFDEVICE` and `WDFREQUEST` handles.
    unsafe extern "system" fn(
        device: WdfDevicePtr,
        request: WdfRequestPtr,
        output_buffer_length: usize,
        input_buffer_length: usize,
        io_control_code: u32,
    ),
>;

/// Callback invoked when asynchronous IddCx adapter initialization completes for `device`.
pub type EvtAdapterReady = Option<
    // SAFETY: Invoked with the owning `WDFDEVICE` handle and completion `NTSTATUS`.
    unsafe extern "system" fn(device: WdfDevicePtr, adapter_init_status: NtStatus),
>;

/// Callback invoked for `EvtIddCxAdapterQueryTargetInfo`.
pub type EvtQueryTargetInfo = Option<
    // SAFETY: Invoked by IddCx with valid `WDFDEVICE` and output `ZeroVddTargetInfo` pointer.
    unsafe extern "system" fn(
        device: WdfDevicePtr,
        connector_index: u32,
        out_info: *mut ZeroVddTargetInfo,
    ) -> NtStatus,
>;

/// Callback invoked for `EvtIddCxParseMonitorDescription` and `EvtIddCxParseMonitorDescription2`.
pub type EvtParseMonitorDescription = Option<
    // SAFETY: Invoked by IddCx with valid EDID pointer (if `edid_size > 0`) and output pointers.
    unsafe extern "system" fn(
        edid_data: *const u8,
        edid_size: u32,
        max_modes: u32,
        out_modes: *mut ZeroVddVideoSignal,
        out_count: *mut u32,
        out_preferred_idx: *mut u32,
    ) -> NtStatus,
>;

/// Callback invoked for `EvtIddCxMonitorGetDefaultDescriptionModes`.
pub type EvtGetDefaultDescriptionModes = Option<
    // SAFETY: Invoked by IddCx with valid `WDFDEVICE`, `IDDCX_MONITOR`, and output pointers.
    unsafe extern "system" fn(
        device: WdfDevicePtr,
        monitor: IddCxMonitorPtr,
        max_modes: u32,
        out_modes: *mut ZeroVddVideoSignal,
        out_count: *mut u32,
        out_preferred_idx: *mut u32,
    ) -> NtStatus,
>;

/// Callback invoked for `EvtIddCxMonitorQueryTargetModes` and `EvtIddCxMonitorQueryTargetModes2`.
pub type EvtQueryTargetModes = Option<
    // SAFETY: Invoked by IddCx with valid `WDFDEVICE`, `IDDCX_MONITOR`, and output pointers.
    unsafe extern "system" fn(
        device: WdfDevicePtr,
        monitor: IddCxMonitorPtr,
        max_modes: u32,
        out_modes: *mut ZeroVddVideoSignal,
        out_count: *mut u32,
    ) -> NtStatus,
>;

/// Callback invoked for `EvtIddCxAdapterCommitModes` and `EvtIddCxAdapterCommitModes2`.
pub type EvtCommitModes = Option<
    // SAFETY: Invoked by IddCx with valid `WDFDEVICE` and `paths` array of length `path_count`.
    unsafe extern "system" fn(
        device: WdfDevicePtr,
        path_count: u32,
        paths: *const ZeroVddCommitPath,
    ) -> NtStatus,
>;

/// Callback invoked for `EvtIddCxMonitorSetDefaultHdrMetaData`.
pub type EvtSetDefaultHdrMetadata = Option<
    // SAFETY: Invoked by IddCx with valid `WDFDEVICE`, `IDDCX_MONITOR`, and optional metadata ptr.
    unsafe extern "system" fn(
        device: WdfDevicePtr,
        monitor: IddCxMonitorPtr,
        metadata_type: u32,
        hdr10: *const ZeroVddHdr10Metadata,
    ) -> NtStatus,
>;

/// Callback invoked for `EvtIddCxMonitorSetGammaRamp`.
pub type EvtSetGammaRamp = Option<
    // SAFETY: Invoked by IddCx with valid `WDFDEVICE` and `IDDCX_MONITOR` handles.
    unsafe extern "system" fn(
        device: WdfDevicePtr,
        monitor: IddCxMonitorPtr,
        ramp_type: u32,
        ramp_size_bytes: u32,
    ) -> NtStatus,
>;

/// Callback invoked for `EvtIddCxMonitorAssignSwapChain`.
pub type EvtAssignSwapChain = Option<
    // SAFETY: Invoked by IddCx with valid handles for monitor, swapchain, and frame event.
    unsafe extern "system" fn(
        device: WdfDevicePtr,
        monitor: IddCxMonitorPtr,
        swapchain: IddCxSwapChainPtr,
        next_surface_available_event: RawHandle,
        render_luid_low: u32,
        render_luid_high: i32,
    ) -> NtStatus,
>;

/// Callback invoked for `EvtIddCxMonitorUnassignSwapChain`.
pub type EvtUnassignSwapChain = Option<
    // SAFETY: Invoked by IddCx with valid `WDFDEVICE` and `IDDCX_MONITOR` handles.
    unsafe extern "system" fn(device: WdfDevicePtr, monitor: IddCxMonitorPtr) -> NtStatus,
>;

/// Complete IddCx callback registration table passed to [`zerovdd_iddcx_device_init_config`].
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ZeroVddIddCxCallbacks {
    /// IOCTL dispatch callback.
    pub evt_io_control: EvtIddCxDeviceIoControl,
    /// Asynchronous adapter-ready completion callback.
    pub evt_adapter_ready: EvtAdapterReady,
    /// Target capability / dithering query callback.
    pub evt_query_target_info: EvtQueryTargetInfo,
    /// Monitor description parser callback.
    pub evt_parse_monitor_description: EvtParseMonitorDescription,
    /// Default EDID-less monitor modes callback.
    pub evt_get_default_description_modes: EvtGetDefaultDescriptionModes,
    /// Monitor target mode query callback.
    pub evt_query_target_modes: EvtQueryTargetModes,
    /// Committed display path notification callback.
    pub evt_commit_modes: EvtCommitModes,
    /// Default HDR10 mastering metadata callback.
    pub evt_set_default_hdr_metadata: EvtSetDefaultHdrMetadata,
    /// Gamma ramp notification callback.
    pub evt_set_gamma_ramp: EvtSetGammaRamp,
    /// Swapchain assignment callback.
    pub evt_assign_swapchain: EvtAssignSwapChain,
    /// Swapchain unassignment callback.
    pub evt_unassign_swapchain: EvtUnassignSwapChain,
}

extern "C" {
    /// Probes runtime IddCx 1.10 function and structure-field availability.
    pub fn zerovdd_iddcx_probe_v110() -> u32;

    /// Returns non-zero if all FP16 / HDR10 callbacks and DDIs are available at runtime.
    pub fn zerovdd_iddcx_supports_fp16(caps: u32) -> u8;

    /// Queries the active runtime IddCx version via `IddCxGetVersion`.
    pub fn zerovdd_iddcx_get_version(out_version: *mut u32) -> NtStatus;

    /// Calls `IddCxDeviceInitConfig` on `device_init` with the supplied callback table.
    pub fn zerovdd_iddcx_device_init_config(
        device_init: WdfDeviceInitPtr,
        callbacks: *const ZeroVddIddCxCallbacks,
    ) -> NtStatus;

    /// Calls `IddCxDeviceInitialize` after `WdfDeviceCreate` completes.
    pub fn zerovdd_iddcx_device_initialize(device: WdfDevicePtr) -> NtStatus;

    /// Calls `IddCxAdapterInitAsync` to register the indirect display adapter.
    pub fn zerovdd_iddcx_adapter_init_async(
        device: WdfDevicePtr,
        max_monitors: u32,
        can_process_fp16: u8,
        out_adapter: *mut IddCxAdapterPtr,
    ) -> NtStatus;

    /// Calls `IddCxAdapterSetRenderAdapter` when supported by the runtime IddCx version.
    pub fn zerovdd_iddcx_adapter_set_render_adapter(
        adapter: IddCxAdapterPtr,
        luid_low: u32,
        luid_high: i32,
    ) -> NtStatus;

    /// Calls `IddCxMonitorCreate` with an EDID descriptor and container GUID.
    pub fn zerovdd_iddcx_monitor_create(
        device: WdfDevicePtr,
        adapter: IddCxAdapterPtr,
        connector_index: u32,
        monitor_guid: *const u8,
        edid_data: *const u8,
        edid_size: u32,
        out_monitor: *mut IddCxMonitorPtr,
    ) -> NtStatus;

    /// Calls `IddCxMonitorArrival` and returns `{OsAdapterLuid, OsTargetId}`.
    pub fn zerovdd_iddcx_monitor_arrival(
        monitor: IddCxMonitorPtr,
        out_luid_low: *mut u32,
        out_luid_high: *mut i32,
        out_target_id: *mut u32,
    ) -> NtStatus;

    /// Calls `IddCxMonitorDeparture` to detach a virtual monitor.
    pub fn zerovdd_iddcx_monitor_departure(monitor: IddCxMonitorPtr) -> NtStatus;

    /// Calls `IddCxMonitorUpdateModes2` (or v1 fallback) to replace a monitor's mode list in place.
    pub fn zerovdd_iddcx_monitor_update_modes(
        monitor: IddCxMonitorPtr,
        use_v2: u8,
        modes: *const ZeroVddVideoSignal,
        mode_count: u32,
    ) -> NtStatus;

    /// Calls `IddCxMonitorSetupHardwareCursor` with the caller's unnamed auto-reset event.
    pub fn zerovdd_iddcx_monitor_setup_hw_cursor(
        monitor: IddCxMonitorPtr,
        cursor_event: RawHandle,
        max_x: u32,
        max_y: u32,
    ) -> NtStatus;

    /// Queries the latest hardware cursor via `IddCxMonitorQueryHardwareCursor3`, `2`, or `1`.
    pub fn zerovdd_iddcx_monitor_query_hw_cursor(
        monitor: IddCxMonitorPtr,
        last_shape_id: u32,
        shape_buffer: *mut u8,
        shape_buffer_size: u32,
        out_info: *mut ZeroVddCursorInfo,
    ) -> NtStatus;

    /// Binds an `IDXGIDevice` to `swapchain` via `IddCxSwapChainSetDevice`.
    pub fn zerovdd_iddcx_swapchain_set_device(
        swapchain: IddCxSwapChainPtr,
        dxgi_device: *mut c_void,
    ) -> HResult;

    /// Acquires the next swapchain buffer (v2 when `use_v2 != 0` and available) and releases `pSurface`.
    pub fn zerovdd_iddcx_swapchain_release_and_acquire_buffer(
        swapchain: IddCxSwapChainPtr,
        use_v2: u8,
        out_frame: *mut ZeroVddFrameInfo,
    ) -> HResult;

    /// Signals frame completion to IddCx via `IddCxSwapChainFinishedProcessingFrame`.
    pub fn zerovdd_iddcx_swapchain_finished_processing_frame(
        swapchain: IddCxSwapChainPtr,
    ) -> HResult;

    /// Creates a hardware D3D11 device + DXGI device + DXGI factory matching `LUID`, skipping WARP.
    pub fn zerovdd_d3d_create_device_for_luid(
        luid_low: u32,
        luid_high: i32,
        out_d3d11_device: *mut *mut c_void,
        out_dxgi_device: *mut *mut c_void,
        out_dxgi_factory: *mut *mut c_void,
    ) -> HResult;

    /// Returns non-zero if `dxgi_factory->IsCurrent()` and `d3d11_device->GetDeviceRemovedReason() == S_OK`.
    pub fn zerovdd_d3d_is_device_valid(d3d11_device: *mut c_void, dxgi_factory: *mut c_void) -> u8;

    /// Clears state and flushes the immediate context on `d3d11_device` to drain deferred destruction.
    pub fn zerovdd_d3d_flush_device(d3d11_device: *mut c_void);

    /// Increments the reference count on a COM `IUnknown` pointer.
    pub fn zerovdd_com_add_ref(com_obj: *mut c_void);

    /// Decrements the reference count on a COM `IUnknown` pointer.
    pub fn zerovdd_com_release(com_obj: *mut c_void);
}
