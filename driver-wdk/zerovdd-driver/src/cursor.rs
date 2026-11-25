//! RAII hardware cursor event handle and IddCx cursor-drain handler.

use alloc::vec;
use alloc::vec::Vec;
use core::ptr;

use wdk_sys::windows::{CloseHandle, CreateEventW};
use wdk_sys::{BOOL, HANDLE};
use zerovdd_iddcx_sys::{
    zerovdd_iddcx_monitor_query_hw_cursor, zerovdd_iddcx_monitor_setup_hw_cursor, IddCxMonitorPtr,
    NtStatus, RawHandle, ZeroVddCursorInfo,
};

use crate::etw::EtwLogger;

const MAX_CURSOR_WIDTH: u32 = 256;
const MAX_CURSOR_HEIGHT: u32 = 256;
const CURSOR_SHAPE_BUFFER_BYTES: usize =
    (MAX_CURSOR_WIDTH as usize) * (MAX_CURSOR_HEIGHT as usize) * 4;

/// RAII wrapper around an unnamed auto-reset Win32 `HANDLE` for IddCx hardware cursor updates.
///
/// Using an unnamed event avoids cross-monitor name collisions and guarantees `CloseHandle`
/// executes deterministically on swapchain unassignment or monitor cleanup.
pub struct HardwareCursorHandle {
    event: HANDLE,
    last_shape_id: u32,
    shape_buffer: Vec<u8>,
}

// SAFETY: Win32 event `HANDLE` and owned shape buffer are moved into the owning swapchain worker
// thread and closed once on `Drop`.
unsafe impl Send for HardwareCursorHandle {}

impl HardwareCursorHandle {
    /// Allocates an unnamed auto-reset event and shape scratch buffer for `monitor`.
    pub fn new() -> Option<Self> {
        // SAFETY: `CreateEventW(NULL, FALSE, FALSE, NULL)` creates an unnamed auto-reset event.
        let event = unsafe { CreateEventW(ptr::null_mut(), 0, 0, ptr::null()) };
        if event.is_null() {
            return None;
        }
        Some(Self {
            event,
            last_shape_id: 0,
            shape_buffer: vec![0u8; CURSOR_SHAPE_BUFFER_BYTES],
        })
    }

    /// Returns the raw Win32 event handle for `WaitForMultipleObjects`.
    #[must_use]
    pub fn event_handle(&self) -> RawHandle {
        self.event.cast()
    }

    /// Registers hardware cursor acceleration for `monitor` using this unnamed event.
    pub fn setup(&self, monitor: IddCxMonitorPtr) -> NtStatus {
        // SAFETY: `monitor` is a valid `IDDCX_MONITOR` and `self.event` stays open until drop.
        unsafe {
            zerovdd_iddcx_monitor_setup_hw_cursor(
                monitor,
                self.event.cast(),
                MAX_CURSOR_WIDTH,
                MAX_CURSOR_HEIGHT,
            )
        }
    }

    /// Drains pending hardware cursor updates via `IddCxMonitorQueryHardwareCursor3`/`2`/`1`.
    pub fn drain(&mut self, monitor: IddCxMonitorPtr, etw: &EtwLogger) -> bool {
        let mut info = ZeroVddCursorInfo::default();
        let buf_len = u32::try_from(self.shape_buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: `monitor`, `shape_buffer`, and `info` are valid for the synchronous query call.
        let status = unsafe {
            zerovdd_iddcx_monitor_query_hw_cursor(
                monitor,
                self.last_shape_id,
                self.shape_buffer.as_mut_ptr(),
                buf_len,
                &mut info,
            )
        };
        if status < 0 {
            etw.log_monitor_event(b"CursorQueryFailed", self.last_shape_id, status);
            return false;
        }
        if info.is_shape_updated != 0 && info.shape_id != 0 {
            self.last_shape_id = info.shape_id;
        }
        true
    }
}

impl Drop for HardwareCursorHandle {
    fn drop(&mut self) {
        if !self.event.is_null() {
            // SAFETY: `self.event` was created by `CreateEventW` and is closed once here.
            unsafe {
                let _closed: BOOL = CloseHandle(self.event);
            }
            self.event = ptr::null_mut();
        }
    }
}
