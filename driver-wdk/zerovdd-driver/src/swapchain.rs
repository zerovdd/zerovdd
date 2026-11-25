//! Per-monitor RAII swapchain worker thread with MMCSS priority, zero polling, and TDR recovery.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

use spin::Mutex;
use wdk_sys::call_unsafe_wdf_function_binding;
use wdk_sys::windows::{
    CloseHandle, CreateEventW, CreateThread, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use wdk_sys::{BOOL, DWORD, HANDLE, WDFOBJECT};
use zerovdd_engine::AdapterLuid;
use zerovdd_iddcx_sys::{
    zerovdd_iddcx_swapchain_finished_processing_frame,
    zerovdd_iddcx_swapchain_release_and_acquire_buffer, zerovdd_iddcx_swapchain_set_device,
    HResult, IddCxMonitorPtr, IddCxSwapChainPtr, RawHandle, ZeroVddFrameInfo,
};

use crate::cursor::HardwareCursorHandle;
use crate::d3d::{is_device_lost_hr, D3D11DeviceManager, DxgiDeviceGuard};
use crate::etw::EtwLogger;

const INFINITE: DWORD = 0xFFFF_FFFF;
const WAIT_OBJECT_0: DWORD = 0;
const E_PENDING: HResult = 0x8000_000A_u32 as i32;

// WHY: UTF-16 NUL-terminated `"DisplayPostProcessing"` MMCSS task name.
static MMCSS_TASK_DISPLAY_POST_PROCESSING: [u16; 22] = [
    b'D' as u16,
    b'i' as u16,
    b's' as u16,
    b'p' as u16,
    b'l' as u16,
    b'a' as u16,
    b'y' as u16,
    b'P' as u16,
    b'o' as u16,
    b's' as u16,
    b't' as u16,
    b'P' as u16,
    b'r' as u16,
    b'o' as u16,
    b'c' as u16,
    b'e' as u16,
    b's' as u16,
    b's' as u16,
    b'i' as u16,
    b'n' as u16,
    b'g' as u16,
    0,
];

extern "system" {
    fn AvSetMmThreadCharacteristicsW(task_name: *const u16, task_index: *mut DWORD) -> HANDLE;
    fn AvRevertMmThreadCharacteristics(avrt_handle: HANDLE) -> BOOL;
    fn SwitchToThread() -> BOOL;
}

struct MmcssGuard {
    handle: HANDLE,
}

impl MmcssGuard {
    fn enter_display_post_processing() -> Self {
        let mut task_index: DWORD = 0;
        // SAFETY: `MMCSS_TASK_DISPLAY_POST_PROCESSING` is static NUL-terminated UTF-16.
        let handle = unsafe {
            AvSetMmThreadCharacteristicsW(
                MMCSS_TASK_DISPLAY_POST_PROCESSING.as_ptr(),
                &mut task_index,
            )
        };
        Self { handle }
    }
}

impl Drop for MmcssGuard {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `self.handle` was returned by `AvSetMmThreadCharacteristicsW`.
            unsafe {
                let _reverted: BOOL = AvRevertMmThreadCharacteristics(self.handle);
            }
            self.handle = ptr::null_mut();
        }
    }
}

/// RAII guard guaranteeing `WdfObjectDelete(hSwapChain)` executes on every worker exit path.
struct SwapChainGuard {
    swapchain: IddCxSwapChainPtr,
}

impl SwapChainGuard {
    const fn new(swapchain: IddCxSwapChainPtr) -> Self {
        Self { swapchain }
    }
}

impl Drop for SwapChainGuard {
    fn drop(&mut self) {
        if !self.swapchain.is_null() {
            // SAFETY: Ownership of `IDDCX_SWAPCHAIN` was transferred to the driver upon
            // `EvtIddCxMonitorAssignSwapChain` returning `STATUS_SUCCESS`.
            unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfObjectDelete,
                    self.swapchain.cast::<c_void>() as WDFOBJECT,
                );
            }
            self.swapchain = ptr::null_mut();
        }
    }
}

struct WorkerArgs {
    monitor: IddCxMonitorPtr,
    swapchain: IddCxSwapChainPtr,
    available_buffer_event: HANDLE,
    terminate_event: HANDLE,
    render_luid: AdapterLuid,
    use_v2: bool,
    d3d: Arc<D3D11DeviceManager>,
    etw: Arc<EtwLogger>,
    cursor: Option<HardwareCursorHandle>,
}

// SAFETY: All pointers inside `WorkerArgs` are Win32/WDF handles or thread-safe `Arc`
// managers synchronized across the lifetime of the joined worker thread.
unsafe impl Send for WorkerArgs {}

/// Owns a per-monitor frame-processing thread and tears it down synchronously on unassign.
pub struct SwapChainWorker {
    thread_handle: HANDLE,
    terminate_event: HANDLE,
}

// SAFETY: `thread_handle` and `terminate_event` are Win32 kernel object handles closed
// exclusively when `stop_and_join` or `Drop` finishes waiting on the thread.
unsafe impl Send for SwapChainWorker {}

impl SwapChainWorker {
    /// Spawns a swapchain worker thread for `monitor` with an optional hardware cursor handle.
    ///
    /// On error before spawning, `swapchain` is NOT deleted so `EvtIddCxMonitorAssignSwapChain`
    /// can return failure and let IddCx clean up the unassigned swapchain object.
    #[allow(
        clippy::too_many_arguments,
        reason = "Transfers full swapchain assignment handles and managers into worker thread"
    )]
    pub fn start(
        monitor: IddCxMonitorPtr,
        swapchain: IddCxSwapChainPtr,
        available_buffer_event: RawHandle,
        render_luid: AdapterLuid,
        use_v2: bool,
        d3d: Arc<D3D11DeviceManager>,
        etw: Arc<EtwLogger>,
    ) -> Option<Self> {
        if monitor.is_null() || swapchain.is_null() || available_buffer_event.is_null() {
            return None;
        }

        // SAFETY: Create an unnamed manual-reset event (`bManualReset = TRUE`, initial `FALSE`).
        let terminate_event = unsafe { CreateEventW(ptr::null_mut(), 1, 0, ptr::null()) };
        if terminate_event.is_null() {
            return None;
        }

        let cursor = HardwareCursorHandle::new();
        let args = Box::new(WorkerArgs {
            monitor,
            swapchain,
            available_buffer_event: available_buffer_event.cast(),
            terminate_event,
            render_luid,
            use_v2,
            d3d,
            etw,
            cursor,
        });
        let raw_args = Box::into_raw(args);

        // SAFETY: `raw_args` is uniquely owned and freed by `swapchain_thread_proc` on start,
        // or reclaimed below if `CreateThread` returns null.
        let thread_handle = unsafe {
            CreateThread(
                ptr::null_mut(),
                0,
                Some(swapchain_thread_proc),
                raw_args.cast::<c_void>(),
                0,
                ptr::null_mut(),
            )
        };
        if thread_handle.is_null() {
            // SAFETY: `CreateThread` failed, so `raw_args` was never passed to a running thread.
            let _reclaimed = unsafe { Box::from_raw(raw_args) };
            // SAFETY: `terminate_event` was just created and has no waiters.
            unsafe {
                let _closed: BOOL = CloseHandle(terminate_event);
            }
            return None;
        }

        Some(Self {
            thread_handle,
            terminate_event,
        })
    }

    /// Signals termination, waits synchronously for thread exit, and closes Win32 handles.
    pub fn stop_and_join(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        if !self.terminate_event.is_null() {
            // SAFETY: `self.terminate_event` remains open until after `WaitForSingleObject` joins.
            unsafe {
                let _signaled: BOOL = SetEvent(self.terminate_event);
            }
        }
        if !self.thread_handle.is_null() {
            // SAFETY: Worker thread never holds WDF/engine locks across exit, so `INFINITE` join
            // completes promptly after `terminate_event` is signaled.
            unsafe {
                let _waited = WaitForSingleObject(self.thread_handle, INFINITE);
                let _closed: BOOL = CloseHandle(self.thread_handle);
            }
            self.thread_handle = ptr::null_mut();
        }
        if !self.terminate_event.is_null() {
            // SAFETY: Worker thread has exited and no longer references `self.terminate_event`.
            unsafe {
                let _closed: BOOL = CloseHandle(self.terminate_event);
            }
            self.terminate_event = ptr::null_mut();
        }
    }
}

impl Drop for SwapChainWorker {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

fn bind_device(args: &WorkerArgs) -> Result<DxgiDeviceGuard, HResult> {
    let dxgi_dev = args.d3d.create_device(args.render_luid)?;
    // SAFETY: `args.swapchain` is a live `IDDCX_SWAPCHAIN` and `dxgi_dev.as_ptr()` is valid.
    let hr = unsafe { zerovdd_iddcx_swapchain_set_device(args.swapchain, dxgi_dev.as_ptr()) };
    if hr < 0 {
        return Err(hr);
    }
    Ok(dxgi_dev)
}

// SAFETY: Registered as `LPTHREAD_START_ROUTINE` for `CreateThread` with a `Box::into_raw`
// `WorkerArgs` allocation owned exclusively by this thread.
unsafe extern "C" fn swapchain_thread_proc(param: *mut c_void) -> DWORD {
    if param.is_null() {
        return 0;
    }
    // SAFETY: `param` was created by `Box::into_raw` in `SwapChainWorker::start`.
    let mut args = unsafe { Box::from_raw(param.cast::<WorkerArgs>()) };

    let _mmcss = MmcssGuard::enter_display_post_processing();
    let _swapchain_guard = SwapChainGuard::new(args.swapchain);

    // WHY: Exit before D3D11 device creation or cursor registration if teardown already
    // signaled `terminate_event` while this thread was starting up.
    // SAFETY: `args.terminate_event` remains open until `stop_inner` joins this thread.
    if unsafe { WaitForSingleObject(args.terminate_event, 0) } == WAIT_OBJECT_0 {
        return 0;
    }

    let mut bound_device = match bind_device(&args) {
        Ok(dev) => Some(dev),
        Err(hr) => {
            args.etw
                .log_monitor_event(b"SwapChainSetDeviceFailed", 0, hr);
            return 0;
        }
    };

    let mut cursor_event: HANDLE = ptr::null_mut();
    if let Some(ref cursor) = args.cursor {
        let cursor_status = cursor.setup(args.monitor);
        if cursor_status >= 0 {
            cursor_event = cursor.event_handle().cast();
        } else {
            args.etw
                .log_monitor_event(b"CursorSetupFailed", 0, cursor_status);
        }
    }

    let wait_handles: [HANDLE; 3] = [
        args.terminate_event,
        args.available_buffer_event,
        cursor_event,
    ];
    let wait_count: DWORD = if cursor_event.is_null() { 2 } else { 3 };
    let mut frame_info = ZeroVddFrameInfo::default();

    'frame_loop: loop {
        // WHY: Non-blocking terminate check before each acquire prevents continuous high-FPS
        // frames from starving shutdown or monitor departure.
        // SAFETY: `args.terminate_event` stays open until after this thread exits.
        if unsafe { WaitForSingleObject(args.terminate_event, 0) } == WAIT_OBJECT_0 {
            break 'frame_loop;
        }

        // WHY: Drain pending hardware cursor events non-blockingly between back-to-back frames
        // so continuous rendering cannot starve cursor updates.
        if !cursor_event.is_null() {
            // SAFETY: `cursor_event` belongs to `args.cursor` and remains open for this thread.
            if unsafe { WaitForSingleObject(cursor_event, 0) } == WAIT_OBJECT_0 {
                if let Some(ref mut cursor) = args.cursor {
                    let _ok = cursor.drain(args.monitor, &args.etw);
                }
            }
        }

        // WHY: `IddCxSwapChainFinishedProcessingFrame` does not release the acquired buffer;
        // calling `ReleaseAndAcquireBuffer[2]` first releases any prior frame and returns
        // `E_PENDING` once idle, arming `available_buffer_event` for zero-polling wait.
        // SAFETY: `args.swapchain` is live and `frame_info` is a stack struct.
        let hr = unsafe {
            zerovdd_iddcx_swapchain_release_and_acquire_buffer(
                args.swapchain,
                u8::from(args.use_v2),
                &mut frame_info,
            )
        };

        if hr == E_PENDING {
            // WHY: No swapchain buffer is held after `E_PENDING`; block with zero polling
            // until terminate, the next desktop surface, or a cursor update arrives.
            loop {
                // SAFETY: `wait_handles[0..wait_count]` are valid handles (`terminate_event` at index 0).
                let signaled = unsafe {
                    WaitForMultipleObjects(wait_count, wait_handles.as_ptr(), 0, INFINITE)
                };
                if signaled == WAIT_OBJECT_0 {
                    break 'frame_loop;
                }
                if wait_count == 3 && signaled == WAIT_OBJECT_0 + 2 {
                    if let Some(ref mut cursor) = args.cursor {
                        let _ok = cursor.drain(args.monitor, &args.etw);
                    }
                    continue;
                }
                if signaled == WAIT_OBJECT_0 + 1 {
                    continue 'frame_loop;
                }
                break 'frame_loop;
            }
        }

        let final_hr = if hr >= 0 {
            // SAFETY: `args.swapchain` acquired a buffer and must report completion before the
            // next `ReleaseAndAcquireBuffer[2]` releases it.
            unsafe { zerovdd_iddcx_swapchain_finished_processing_frame(args.swapchain) }
        } else {
            hr
        };

        if final_hr < 0 {
            if is_device_lost_hr(final_hr) {
                args.etw
                    .log_tdr_recovery(args.render_luid.low, args.render_luid.high, final_hr);
                drop(bound_device.take());
                match bind_device(&args) {
                    Ok(new_dev) => {
                        bound_device = Some(new_dev);
                        continue 'frame_loop;
                    }
                    Err(rebind_hr) => {
                        args.etw
                            .log_monitor_event(b"SwapChainRebindFailed", 0, rebind_hr);
                        break 'frame_loop;
                    }
                }
            }
            args.etw
                .log_monitor_event(b"SwapChainFrameFailed", 0, final_hr);
            break 'frame_loop;
        }
    }

    // WHY: Delete `IDDCX_SWAPCHAIN` before dropping `DxgiDeviceGuard` so IddCx releases its
    // internal buffer references prior to `ClearState`/`Flush` and `ID3D11Device::Release`.
    drop(_swapchain_guard);
    drop(bound_device);
    0
}

struct RegistryState {
    workers: Vec<(usize, SwapChainWorker)>,
    departing: Vec<usize>,
    in_flight: Vec<usize>,
}

/// RAII guard reserving an in-flight swapchain assignment so departure drains it synchronously.
pub struct AssignGuard<'a> {
    registry: &'a SwapChainRegistry,
    key: usize,
    finished: bool,
}

impl AssignGuard<'_> {
    /// Installs `worker` unless departing, joins any replaced or rejected worker, and releases reservation.
    #[must_use]
    pub fn commit(mut self, worker: SwapChainWorker) -> bool {
        let (accepted, to_stop) = {
            let mut guard = self.registry.state.lock();
            if guard.departing.contains(&self.key) {
                (false, Some(worker))
            } else {
                let prev = guard
                    .workers
                    .iter()
                    .position(|(k, _)| *k == self.key)
                    .map(|idx| guard.workers.swap_remove(idx).1);
                guard.workers.push((self.key, worker));
                (true, prev)
            }
        };
        if let Some(old_or_rejected) = to_stop {
            old_or_rejected.stop_and_join();
        }
        self.clear_in_flight();
        accepted
    }

    fn clear_in_flight(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.registry.remove_in_flight(self.key);
    }
}

impl Drop for AssignGuard<'_> {
    fn drop(&mut self) {
        self.clear_in_flight();
    }
}

/// Device-scoped table of active [`SwapChainWorker`] instances keyed by `IDDCX_MONITOR`.
pub struct SwapChainRegistry {
    state: Mutex<RegistryState>,
}

impl SwapChainRegistry {
    /// Creates an empty [`SwapChainRegistry`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                workers: Vec::new(),
                departing: Vec::new(),
                in_flight: Vec::new(),
            }),
        }
    }

    fn remove_in_flight(&self, key: usize) {
        let mut guard = self.state.lock();
        if let Some(idx) = guard.in_flight.iter().position(|k| *k == key) {
            guard.in_flight.swap_remove(idx);
        }
    }

    /// Reserves an assignment slot for `monitor`, returning `None` if it is already departing.
    #[must_use]
    pub fn try_begin_assign(&self, monitor: IddCxMonitorPtr) -> Option<AssignGuard<'_>> {
        let key = monitor as usize;
        let mut guard = self.state.lock();
        if guard.departing.contains(&key) {
            None
        } else {
            guard.in_flight.push(key);
            Some(AssignGuard {
                registry: self,
                key,
                finished: false,
            })
        }
    }

    /// Marks `monitor` departing and waits until all active and in-flight workers are joined.
    pub fn begin_depart_and_drain(&self, monitor: IddCxMonitorPtr) {
        let key = monitor as usize;
        {
            let mut guard = self.state.lock();
            if !guard.departing.contains(&key) {
                guard.departing.push(key);
            }
        }
        loop {
            self.remove_and_stop(monitor);
            let done = {
                let guard = self.state.lock();
                !guard.in_flight.contains(&key) && !guard.workers.iter().any(|(k, _)| *k == key)
            };
            if done {
                break;
            }
            // SAFETY: `SwitchToThread` yields the CPU slice cleanly with no pointer arguments.
            unsafe {
                let _yielded = SwitchToThread();
            }
            core::hint::spin_loop();
        }
    }

    /// Clears departing state for `monitor` and synchronously stops any remaining worker.
    pub fn finish_depart(&self, monitor: IddCxMonitorPtr) {
        let key = monitor as usize;
        {
            let mut guard = self.state.lock();
            guard.departing.retain(|k| *k != key);
        }
        self.remove_and_stop(monitor);
    }

    /// Removes and synchronously stops the worker associated with `monitor`, if present.
    pub fn remove_and_stop(&self, monitor: IddCxMonitorPtr) {
        let key = monitor as usize;
        let removed = {
            let mut guard = self.state.lock();
            let popped = guard
                .workers
                .iter()
                .position(|(k, _)| *k == key)
                .map(|idx| guard.workers.swap_remove(idx).1);
            if popped.is_some() {
                guard.in_flight.push(key);
            }
            popped
        };
        if let Some(worker) = removed {
            worker.stop_and_join();
            self.remove_in_flight(key);
        }
    }

    /// Removes and synchronously stops all active swapchain workers (called in `ReleaseHardware`).
    pub fn stop_all(&self) {
        let drained = {
            let mut guard = self.state.lock();
            guard.departing.clear();
            core::mem::take(&mut guard.workers)
        };
        for (_, worker) in drained {
            worker.stop_and_join();
        }
    }
}

impl Default for SwapChainRegistry {
    fn default() -> Self {
        Self::new()
    }
}
