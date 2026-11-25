//! Control interface gate (`\ZeroVddControl`), file lifecycle, and IOCTL routing.

use alloc::vec;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use wdk_sys::call_unsafe_wdf_function_binding;
use wdk_sys::{
    GUID, NTSTATUS, PCUNICODE_STRING, PVOID, ULONG_PTR, UNICODE_STRING, WDFDEVICE, WDFFILEOBJECT,
    WDFREQUEST,
};
use zerovdd_engine::{dispatch_ioctl, EngineError, FileKey};
use zerovdd_proto::ioctl::{
    IOCTL_ADD_DISPLAY, IOCTL_GET_STATUS, IOCTL_GET_VERSION, IOCTL_LIST_DISPLAYS,
    IOCTL_REMOVE_DISPLAY, IOCTL_RENEW_LEASE, IOCTL_SET_RENDER_ADAPTER, IOCTL_UPDATE_DISPLAY_MODE,
    ZEROVDD_CONTROL_INTERFACE_GUID,
};
use zerovdd_proto::validation::ProtoError;

use crate::iddcx::WdfIddPlatform;
use crate::security;
use crate::wdf::{
    get_device_runtime, get_file_context, STATUS_ACCESS_DENIED, STATUS_BUFFER_TOO_SMALL,
    STATUS_CANCELLED, STATUS_INSUFFICIENT_RESOURCES, STATUS_INVALID_DEVICE_REQUEST,
    STATUS_INVALID_HANDLE, STATUS_INVALID_PARAMETER, STATUS_NOT_FOUND,
    STATUS_OBJECT_NAME_COLLISION, STATUS_SUCCESS, STATUS_UNSUCCESSFUL,
};
use zerovdd_iddcx_sys::{WdfDevicePtr, WdfRequestPtr};

/// Device interface GUID exposed with `ReferenceString = "ZeroVddControl"`.
pub const CONTROL_INTERFACE_GUID: GUID = GUID {
    Data1: ZEROVDD_CONTROL_INTERFACE_GUID.data1,
    Data2: ZEROVDD_CONTROL_INTERFACE_GUID.data2,
    Data3: ZEROVDD_CONTROL_INTERFACE_GUID.data3,
    Data4: ZEROVDD_CONTROL_INTERFACE_GUID.data4,
};

// WHY: UTF-16 NUL-terminated reference string `"ZeroVddControl"` appended to the Control Interface path.
static CONTROL_REF_BUF: [u16; 15] = [
    b'Z' as u16,
    b'e' as u16,
    b'r' as u16,
    b'o' as u16,
    b'V' as u16,
    b'd' as u16,
    b'd' as u16,
    b'C' as u16,
    b'o' as u16,
    b'n' as u16,
    b't' as u16,
    b'r' as u16,
    b'o' as u16,
    b'l' as u16,
    0,
];

const CONTROL_REF_ASCII: &[u8; 14] = b"ZeroVddControl";
const MAX_STACK_IOCTL_INPUT: usize = 1024;
const MAX_IOCTL_INPUT_LEN: usize = 16384;

/// Per-handle context stored on `WDFFILEOBJECT` to enforce Control Interface authorization.
#[repr(C)]
pub struct FileContext {
    /// `true` when opened against the `\ZeroVddControl` reference string.
    pub is_control: bool,
    /// `true` when the opening caller satisfied `KernelMode`, `SYSTEM`, or `Administrators`.
    pub authorized: bool,
    /// Set atomically in `EvtFileCleanup` before handle-bound monitors are departed.
    pub cleaned_up: AtomicBool,
    /// Number of active `EvtIddCxDeviceIoControl` calls running on this handle.
    pub in_flight_ioctls: AtomicU32,
}

struct InFlightIoctlGuard<'a>(&'a AtomicU32);

impl Drop for InFlightIoctlGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

const fn is_known_ioctl(code: u32) -> bool {
    matches!(
        code,
        IOCTL_GET_VERSION
            | IOCTL_ADD_DISPLAY
            | IOCTL_REMOVE_DISPLAY
            | IOCTL_LIST_DISPLAYS
            | IOCTL_SET_RENDER_ADAPTER
            | IOCTL_RENEW_LEASE
            | IOCTL_GET_STATUS
            | IOCTL_UPDATE_DISPLAY_MODE
    )
}

/// Builds a `UNICODE_STRING` pointing to the static `"ZeroVddControl"` reference buffer.
#[must_use]
pub fn control_reference_unicode_string() -> UNICODE_STRING {
    let char_count = CONTROL_REF_BUF.len().saturating_sub(1);
    UNICODE_STRING {
        Length: (char_count * 2) as u16,
        MaximumLength: (CONTROL_REF_BUF.len() * 2) as u16,
        Buffer: CONTROL_REF_BUF.as_ptr().cast_mut(),
    }
}

// SAFETY: `file_name` must be null or point to a valid `UNICODE_STRING` returned by WDF.
unsafe fn is_control_reference_name(file_name: PCUNICODE_STRING) -> bool {
    if file_name.is_null() {
        return false;
    }
    // SAFETY: Caller guarantees `file_name` points to an initialized `UNICODE_STRING`.
    let (byte_len, buf_ptr) = unsafe { ((*file_name).Length as usize, (*file_name).Buffer) };
    if buf_ptr.is_null() || byte_len == 0 || !byte_len.is_multiple_of(2) {
        return false;
    }
    // SAFETY: `buf_ptr` is valid for `byte_len / 2` UTF-16 code units per `UNICODE_STRING`.
    let units = unsafe { core::slice::from_raw_parts(buf_ptr, byte_len / 2) };
    let trimmed = match units.first().copied() {
        Some(ch) if ch == u16::from(b'\\') => match units.get(1..) {
            Some(rest) => rest,
            None => return false,
        },
        _ => units,
    };
    if trimmed.len() != CONTROL_REF_ASCII.len() {
        return false;
    }
    trimmed
        .iter()
        .zip(CONTROL_REF_ASCII.iter())
        .all(|(&unit, &expected)| {
            u8::try_from(unit).is_ok_and(|ascii| ascii.eq_ignore_ascii_case(&expected))
        })
}

// SAFETY: Registered as `EVT_WDF_REQUEST_IMPERSONATE`; `context` points to a stack `bool`.
unsafe extern "C" fn evt_request_impersonate(_request: WDFREQUEST, context: PVOID) {
    if context.is_null() {
        return;
    }
    // SAFETY: Invoked inside `WdfRequestImpersonate` with caller token active on thread.
    let authorized = unsafe { security::is_impersonated_caller_system_or_admin() };
    // SAFETY: `context` points to a mutable `bool` on `evt_device_file_create`'s stack.
    unsafe {
        *context.cast::<bool>() = authorized;
    }
}

/// WDF `EvtDeviceFileCreate` callback gating `\ZeroVddControl` to `SYSTEM` / `Administrators`.
///
/// Non-control opens (Display / DXGI / DWM) complete with `STATUS_SUCCESS` and `is_control = false`.
///
/// # Safety
///
/// Must only be registered with WDF and called with valid `WDFDEVICE`, `WDFREQUEST`, and `WDFFILEOBJECT`.
// SAFETY: Registered as `EVT_WDF_DEVICE_FILE_CREATE`; WDF guarantees valid handles.
pub unsafe extern "C" fn evt_device_file_create(
    _device: WDFDEVICE,
    request: WDFREQUEST,
    file_object: WDFFILEOBJECT,
) {
    // SAFETY: `file_object` is the valid create target passed to `EvtDeviceFileCreate`.
    let file_name =
        unsafe { call_unsafe_wdf_function_binding!(WdfFileObjectGetFileName, file_object) };
    // SAFETY: `file_name` was returned by `WdfFileObjectGetFileName`.
    let is_control = unsafe { is_control_reference_name(file_name) };

    if !is_control {
        // SAFETY: `file_object` carries a `FileContext`.
        let file_ctx = unsafe { get_file_context(file_object) };
        if !file_ctx.is_null() {
            // SAFETY: Non-control opens (DWM/DXGI/system) keep standard `Class=Display` access.
            unsafe {
                (*file_ctx).is_control = false;
                (*file_ctx).authorized = false;
                (*file_ctx).cleaned_up = AtomicBool::new(false);
                (*file_ctx).in_flight_ioctls = AtomicU32::new(0);
            }
        }
        // SAFETY: Completing non-control open with `STATUS_SUCCESS`.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestComplete, request, STATUS_SUCCESS);
        }
        return;
    }

    // SAFETY: `request` is the active create request passed to `EvtDeviceFileCreate`.
    let mode = unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetRequestorMode, request) };

    let authorized = if i32::from(mode) == wdk_sys::_MODE::KernelMode {
        true
    } else {
        let mut is_priv = false;
        // SAFETY: `request` is a valid create request; `is_priv` lives across synchronous call.
        let imp_status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestImpersonate,
                request,
                wdk_sys::_SECURITY_IMPERSONATION_LEVEL::SecurityIdentification,
                Some(evt_request_impersonate),
                (&mut is_priv as *mut bool).cast::<c_void>(),
            )
        };
        imp_status >= 0 && is_priv
    };

    if !authorized {
        // SAFETY: Completing unauthorized control-interface open with `STATUS_ACCESS_DENIED`.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestComplete, request, STATUS_ACCESS_DENIED);
        }
        return;
    }

    // SAFETY: `file_object` carries a `FileContext`.
    let file_ctx = unsafe { get_file_context(file_object) };
    if !file_ctx.is_null() {
        // SAFETY: `file_ctx` points to valid `FileContext` memory owned by WDF.
        unsafe {
            (*file_ctx).is_control = true;
            (*file_ctx).authorized = true;
            (*file_ctx).cleaned_up = AtomicBool::new(false);
            (*file_ctx).in_flight_ioctls = AtomicU32::new(0);
        }
    }

    // SAFETY: Completing authorized control-interface open with `STATUS_SUCCESS`.
    unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestComplete, request, STATUS_SUCCESS);
    }
}

/// WDF `EvtFileCleanup` callback departing handle-bound virtual monitors for closed control handles.
///
/// # Safety
///
/// Must only be registered with WDF and called with a valid `WDFFILEOBJECT`.
// SAFETY: Registered as `EVT_WDF_FILE_CLEANUP`; `file_object` is valid until return.
pub unsafe extern "C" fn evt_file_cleanup(file_object: WDFFILEOBJECT) {
    // SAFETY: `file_object` carries a `FileContext`.
    let file_ctx = unsafe { get_file_context(file_object) };
    if file_ctx.is_null() {
        return;
    }
    // SAFETY: `file_ctx` is valid while `WDFFILEOBJECT` is open; `SeqCst` pairs with
    // `evt_iddcx_device_io_control` incrementing `in_flight_ioctls` before checking `cleaned_up`.
    let (is_control, authorized, already_cleaned) = unsafe {
        (
            (*file_ctx).is_control,
            (*file_ctx).authorized,
            (*file_ctx).cleaned_up.swap(true, Ordering::SeqCst),
        )
    };
    if !is_control || !authorized || already_cleaned {
        return;
    }

    // WHY: Drain all in-flight IOCTLs on this file handle before departing its monitors
    // so an overlapping `ADD_DISPLAY` cannot race past `on_file_cleanup`.
    // SAFETY: `file_ctx` remains valid until `EvtFileClose` after `EvtFileCleanup` returns.
    while unsafe { (*file_ctx).in_flight_ioctls.load(Ordering::SeqCst) } != 0 {
        core::hint::spin_loop();
    }

    // SAFETY: `file_object` is valid and associated with our `WDFDEVICE`.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfFileObjectGetDevice, file_object) };
    if let Some(runtime) = get_device_runtime(device.cast()) {
        let platform = WdfIddPlatform::new(&runtime);
        let file_key = FileKey(file_object as usize as u64);
        match runtime.engine.on_file_cleanup(file_key, &platform) {
            Ok(departed) => {
                if departed > 0 {
                    runtime.etw.log_counter_u64(
                        b"FileCleanupDeparted",
                        b"departed_count",
                        departed as u64,
                    );
                }
            }
            Err(_) => {
                runtime
                    .etw
                    .log_ioctl_error(0, STATUS_UNSUCCESSFUL, b"file_cleanup_depart_failed");
            }
        }
    }
}

/// WDF `EvtFileClose` callback.
///
/// # Safety
///
/// Must only be registered with WDF and called with a valid `WDFFILEOBJECT`.
// SAFETY: Registered as `EVT_WDF_FILE_CLOSE`; `file_object` is valid until return.
pub unsafe extern "C" fn evt_file_close(_file_object: WDFFILEOBJECT) {}

/// IddCx `EvtIddCxDeviceIoControl` callback validating handle authorization and routing all 7 IOCTLs.
///
/// # Safety
///
/// Must only be registered via `IddCxDeviceInitConfig` and called with valid WDF handles.
// SAFETY: Registered as `EVT_IDD_CX_DEVICE_IO_CONTROL`; `device` and `request` are valid handles.
pub unsafe extern "system" fn evt_iddcx_device_io_control(
    device: WdfDevicePtr,
    request: WdfRequestPtr,
    output_buffer_length: usize,
    input_buffer_length: usize,
    io_control_code: u32,
) {
    let wdf_request = request.cast::<wdk_sys::WDFREQUEST__>();

    // SAFETY: `wdf_request` is an active IOCTL request.
    let file_object =
        unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetFileObject, wdf_request) };
    if file_object.is_null() {
        // SAFETY: Rejecting IOCTL with no associated `WDFFILEOBJECT`.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestComplete,
                wdf_request,
                STATUS_ACCESS_DENIED,
            );
        }
        return;
    }

    // SAFETY: `file_object` carries a `FileContext`.
    let file_ctx = unsafe { get_file_context(file_object) };
    if file_ctx.is_null() {
        // SAFETY: Rejecting IOCTL with missing `FileContext`.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestComplete,
                wdf_request,
                STATUS_ACCESS_DENIED,
            );
        }
        return;
    }

    // WHY: Increment `in_flight_ioctls` (`SeqCst`) BEFORE checking `cleaned_up` (`SeqCst`) so
    // `evt_file_cleanup` cannot miss an entering IOCTL after setting `cleaned_up = true`.
    // SAFETY: `file_ctx` stays valid until `EvtFileClose`, and `evt_file_cleanup` waits for
    // `in_flight_ioctls == 0` before returning.
    let in_flight_ref = unsafe { &(*file_ctx).in_flight_ioctls };
    in_flight_ref.fetch_add(1, Ordering::SeqCst);
    let _in_flight_guard = InFlightIoctlGuard(in_flight_ref);

    // SAFETY: `file_ctx` is non-null and owned by WDF for the lifetime of `file_object`.
    let is_authorized_control = unsafe {
        (*file_ctx).is_control
            && (*file_ctx).authorized
            && !(*file_ctx).cleaned_up.load(Ordering::SeqCst)
    };
    if !is_authorized_control {
        // SAFETY: Rejecting IOCTL issued on non-control, unauthorized, or cleaned-up handle.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestComplete,
                wdf_request,
                STATUS_ACCESS_DENIED,
            );
        }
        return;
    }

    let Some(runtime) = get_device_runtime(device) else {
        // SAFETY: Device runtime is unavailable during teardown.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestComplete,
                wdf_request,
                STATUS_INVALID_DEVICE_REQUEST,
            );
        }
        return;
    };

    if !is_known_ioctl(io_control_code) {
        runtime.etw.log_ioctl_error(
            io_control_code,
            STATUS_INVALID_DEVICE_REQUEST,
            b"unknown_ioctl",
        );
        // SAFETY: Completing unrecognized IOCTL immediately before buffer retrieval.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestComplete,
                wdf_request,
                STATUS_INVALID_DEVICE_REQUEST,
            );
        }
        return;
    }

    if input_buffer_length > MAX_IOCTL_INPUT_LEN {
        runtime.etw.log_ioctl_error(
            io_control_code,
            STATUS_INVALID_PARAMETER,
            b"input_too_large",
        );
        // SAFETY: Rejecting oversized input buffer before retrieving or allocating memory.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestComplete,
                wdf_request,
                STATUS_INVALID_PARAMETER,
            );
        }
        return;
    }

    // WHY: Copy input buffer before borrowing the output buffer because `METHOD_BUFFERED`
    // aliases input and output to the same `AssociatedIrp.SystemBuffer` allocation.
    let mut stack_in = [0u8; MAX_STACK_IOCTL_INPUT];
    let mut heap_in = vec::Vec::new();
    let input_bytes: &[u8] = if input_buffer_length == 0 {
        &[]
    } else {
        let mut in_ptr: PVOID = ptr::null_mut();
        let mut actual_in: usize = 0;
        // SAFETY: `wdf_request` is active and `0 < input_buffer_length <= MAX_IOCTL_INPUT_LEN`.
        let in_status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestRetrieveInputBuffer,
                wdf_request,
                input_buffer_length,
                &mut in_ptr,
                &mut actual_in,
            )
        };
        if in_status < 0 || in_ptr.is_null() || actual_in < input_buffer_length {
            let status = if in_status < 0 {
                in_status
            } else {
                STATUS_BUFFER_TOO_SMALL
            };
            runtime
                .etw
                .log_ioctl_error(io_control_code, status, b"retrieve_input_buffer");
            // SAFETY: Completing request after input buffer retrieval failure.
            unsafe {
                call_unsafe_wdf_function_binding!(WdfRequestComplete, wdf_request, status);
            }
            return;
        }
        // SAFETY: `in_ptr` is readable for `actual_in` bytes while `wdf_request` is valid.
        let raw_in = unsafe { core::slice::from_raw_parts(in_ptr.cast::<u8>(), actual_in) };
        if actual_in <= MAX_STACK_IOCTL_INPUT {
            if let Some(dst) = stack_in.get_mut(..actual_in) {
                dst.copy_from_slice(raw_in);
                dst
            } else {
                &[]
            }
        } else {
            heap_in.extend_from_slice(raw_in);
            heap_in.as_slice()
        }
    };

    let mut empty_out = [0u8; 0];
    let output_slice: &mut [u8] = if output_buffer_length == 0 {
        &mut empty_out
    } else {
        let mut out_ptr: PVOID = ptr::null_mut();
        let mut actual_out: usize = 0;
        // SAFETY: `wdf_request` is active and `output_buffer_length > 0`.
        let out_status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestRetrieveOutputBuffer,
                wdf_request,
                output_buffer_length,
                &mut out_ptr,
                &mut actual_out,
            )
        };
        if out_status < 0 || out_ptr.is_null() || actual_out < output_buffer_length {
            let status = if out_status < 0 {
                out_status
            } else {
                STATUS_BUFFER_TOO_SMALL
            };
            runtime
                .etw
                .log_ioctl_error(io_control_code, status, b"retrieve_output_buffer");
            // SAFETY: Completing request after output buffer retrieval failure.
            unsafe {
                call_unsafe_wdf_function_binding!(WdfRequestComplete, wdf_request, status);
            }
            return;
        }
        // SAFETY: `out_ptr` is writable for `actual_out` bytes and input was copied out above.
        unsafe { core::slice::from_raw_parts_mut(out_ptr.cast::<u8>(), actual_out) }
    };

    let file_key = FileKey(file_object as usize as u64);
    let platform = WdfIddPlatform::new(&runtime);
    if matches!(
        io_control_code,
        IOCTL_ADD_DISPLAY
            | IOCTL_LIST_DISPLAYS
            | IOCTL_RENEW_LEASE
            | IOCTL_GET_STATUS
            | IOCTL_UPDATE_DISPLAY_MODE
    ) {
        let _ = runtime.engine.reap_expired_leases(&platform);
    }
    match dispatch_ioctl(
        &runtime.engine,
        file_key,
        io_control_code,
        input_bytes,
        output_slice,
        &platform,
    ) {
        Ok(bytes_written) => {
            // SAFETY: `bytes_written` bytes were written to `output_slice` within buffer bounds.
            unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfRequestCompleteWithInformation,
                    wdf_request,
                    STATUS_SUCCESS,
                    bytes_written as ULONG_PTR,
                );
            }
        }
        Err(err) => {
            let (status, label) = map_engine_error(&err);
            runtime.etw.log_ioctl_error(io_control_code, status, label);
            // SAFETY: Completing failed/invalid IOCTL with mapped `NTSTATUS`.
            unsafe {
                call_unsafe_wdf_function_binding!(WdfRequestComplete, wdf_request, status);
            }
        }
    }
}

fn map_engine_error(err: &EngineError) -> (NTSTATUS, &'static [u8]) {
    match err {
        EngineError::UnsupportedIoctl(_) => (STATUS_INVALID_DEVICE_REQUEST, b"unsupported_ioctl"),
        EngineError::Proto(ProtoError::BufferTooShort { .. }) => {
            (STATUS_BUFFER_TOO_SMALL, b"buffer_too_small")
        }
        EngineError::Proto(_) => (STATUS_INVALID_PARAMETER, b"proto_validation"),
        EngineError::Edid(_) => (STATUS_INVALID_PARAMETER, b"edid_validation"),
        EngineError::Mode(_) => (STATUS_INVALID_PARAMETER, b"mode_validation"),
        EngineError::DimensionsOutOfRange { .. } => {
            (STATUS_INVALID_PARAMETER, b"dimensions_out_of_range")
        }
        EngineError::NotLeased => (STATUS_INVALID_PARAMETER, b"not_leased"),
        EngineError::MonitorNotFound => (STATUS_NOT_FOUND, b"monitor_not_found"),
        EngineError::MonitorConflict => (STATUS_OBJECT_NAME_COLLISION, b"monitor_conflict"),
        EngineError::NoFreeConnectorSlots { .. } => {
            (STATUS_INSUFFICIENT_RESOURCES, b"no_free_slots")
        }
        EngineError::LeaseExpired => (STATUS_INVALID_HANDLE, b"lease_expired"),
        EngineError::CancelledDuringArrival => (STATUS_CANCELLED, b"cancelled_during_arrival"),
        EngineError::Platform(plat) => {
            use zerovdd_engine::PlatformError;
            let code = match plat {
                PlatformError::InitAdapterFailed { code }
                | PlatformError::CreateMonitorFailed { code, .. }
                | PlatformError::ArriveMonitorFailed { code, .. }
                | PlatformError::DepartMonitorFailed { code, .. }
                | PlatformError::SetRenderAdapterFailed { code, .. }
                | PlatformError::UpdateMonitorModesFailed { code, .. } => *code,
            };
            let nt = if code < 0 { code } else { STATUS_UNSUCCESSFUL };
            (nt, b"platform_error")
        }
    }
}
