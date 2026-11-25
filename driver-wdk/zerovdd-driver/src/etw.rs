//! Structured ETW TraceLogging provider (`ZeroVdd.Driver`) for `WUDFHost.exe`.

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use wdk_sys::GUID;

// WHY: TraceLogging provider GUID `{06714745-69DC-41FF-9684-C532D5183A74}`.
const PROVIDER_GUID: GUID = GUID {
    Data1: 0x0671_4745,
    Data2: 0x69DC,
    Data3: 0x41FF,
    Data4: [0x96, 0x84, 0xC5, 0x32, 0xD5, 0x18, 0x3A, 0x74],
};

// WHY: Provider trait blob `[u16 size_le][b"ZeroVdd.Driver\0"]` (17 bytes total).
const PROVIDER_METADATA: [u8; 17] = [
    17, 0, b'Z', b'e', b'r', b'o', b'V', b'd', b'd', b'.', b'D', b'r', b'i', b'v', b'e', b'r', 0,
];

const EVENT_INFO_SET_PROVIDER_TRAITS: u32 = 2;
const EVENT_DATA_DESCRIPTOR_TYPE_EVENT_METADATA: u32 = 1;
const EVENT_DATA_DESCRIPTOR_TYPE_PROVIDER_METADATA: u32 = 2;

const TLG_IN_ANSISTRING: u8 = 2;
const TLG_IN_INT32: u8 = 7;
const TLG_IN_UINT32: u8 = 8;
const TLG_IN_UINT64: u8 = 10;
const TLG_IN_HEXINT32: u8 = 20;

const ETW_LEVEL_ERROR: u8 = 2;
const ETW_LEVEL_WARN: u8 = 3;
const ETW_LEVEL_INFO: u8 = 4;

#[repr(C)]
struct EventDescriptor {
    id: u16,
    version: u8,
    channel: u8,
    level: u8,
    opcode: u8,
    task: u16,
    keyword: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventDataDescriptor {
    ptr: u64,
    size: u32,
    reserved: u32,
}

impl EventDataDescriptor {
    fn from_bytes(bytes: &[u8], desc_type: u32) -> Self {
        Self {
            ptr: bytes.as_ptr() as usize as u64,
            size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            reserved: desc_type,
        }
    }

    fn from_val<T>(val: &T) -> Self {
        Self {
            ptr: (val as *const T) as usize as u64,
            size: size_of::<T>() as u32,
            reserved: 0,
        }
    }
}

extern "system" {
    fn EventRegister(
        provider_id: *const GUID,
        enable_callback: *const c_void,
        callback_context: *mut c_void,
        reg_handle: *mut u64,
    ) -> u32;

    fn EventSetInformation(
        reg_handle: u64,
        information_class: u32,
        event_information: *const c_void,
        information_length: u32,
    ) -> u32;

    fn EventWriteTransfer(
        reg_handle: u64,
        event_descriptor: *const EventDescriptor,
        activity_id: *const GUID,
        related_activity_id: *const GUID,
        user_data_count: u32,
        user_data: *const EventDataDescriptor,
    ) -> u32;

    fn EventUnregister(reg_handle: u64) -> u32;
}

/// Device-scoped ETW TraceLogging provider (`ZeroVdd.Driver`).
///
/// Unregisters automatically on drop inside `EvtDeviceReleaseHardware` so no
/// global state persists across driver unload.
pub struct EtwLogger {
    reg_handle: AtomicU64,
}

impl EtwLogger {
    /// Registers the `ZeroVdd.Driver` ETW provider with Windows TraceLogging metadata.
    #[must_use]
    pub fn new() -> Self {
        let mut handle: u64 = 0;
        // SAFETY: `PROVIDER_GUID` and `handle` are valid stack/static references for `EventRegister`.
        let status =
            unsafe { EventRegister(&PROVIDER_GUID, ptr::null(), ptr::null_mut(), &mut handle) };
        if status == 0 && handle != 0 {
            // SAFETY: `handle` is registered and `PROVIDER_METADATA` lives for the call.
            let _ = unsafe {
                EventSetInformation(
                    handle,
                    EVENT_INFO_SET_PROVIDER_TRAITS,
                    PROVIDER_METADATA.as_ptr().cast::<c_void>(),
                    PROVIDER_METADATA.len() as u32,
                )
            };
        } else {
            handle = 0;
        }
        Self {
            reg_handle: AtomicU64::new(handle),
        }
    }

    /// Emits an adapter lifecycle initialization or power transition event.
    pub fn log_adapter_event(&self, event_name: &[u8], status: i32, detail: u32) {
        let level = if status < 0 {
            ETW_LEVEL_ERROR
        } else {
            ETW_LEVEL_INFO
        };
        self.write_hex_u32_event(
            level,
            event_name,
            b"status",
            status as u32,
            b"detail",
            detail,
        );
    }

    /// Emits a monitor arrival, departure, or mode negotiation event.
    pub fn log_monitor_event(&self, event_name: &[u8], slot_or_id: u32, status: i32) {
        let level = if status < 0 {
            ETW_LEVEL_ERROR
        } else {
            ETW_LEVEL_INFO
        };
        self.write_u32_hex_event(
            level,
            event_name,
            b"target_or_slot",
            slot_or_id,
            b"status",
            status as u32,
        );
    }

    /// Emits a committed mode path event.
    pub fn log_mode_commit(&self, width: u32, height: u32, color_space: u32, bpc: u32) {
        self.write_four_u32_event(
            ETW_LEVEL_INFO,
            b"CommitMode",
            b"width",
            width,
            b"height",
            height,
            b"color_space",
            color_space,
            b"bpc",
            bpc,
        );
    }

    /// Emits a GPU render adapter switch or circuit-breaker event.
    pub fn log_gpu_switch(&self, event_name: &[u8], luid_low: u32, luid_high: i32, status: i32) {
        let level = if status < 0 {
            ETW_LEVEL_WARN
        } else {
            ETW_LEVEL_INFO
        };
        self.write_gpu_event(level, event_name, luid_low, luid_high, status);
    }

    /// Emits a GPU TDR or DXGI device removal recovery event.
    pub fn log_tdr_recovery(&self, luid_low: u32, luid_high: i32, hresult: i32) {
        self.write_gpu_event(ETW_LEVEL_WARN, b"TdrRecovery", luid_low, luid_high, hresult);
    }

    /// Emits an IOCTL validation or dispatch error event.
    pub fn log_ioctl_error(&self, ioctl_code: u32, status: i32, detail: &[u8]) {
        self.write_ioctl_error_event(ioctl_code, status, detail);
    }

    fn write_hex_u32_event(
        &self,
        level: u8,
        event_name: &[u8],
        f1: &[u8],
        v1: u32,
        f2: &[u8],
        v2: u32,
    ) {
        let handle = self.reg_handle.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let mut meta = [0u8; 128];
        let Some(meta_len) = build_event_metadata(
            &mut meta,
            event_name,
            &[(f1, TLG_IN_HEXINT32), (f2, TLG_IN_UINT32)],
        ) else {
            return;
        };
        let Some(meta_slice) = meta.get(..meta_len) else {
            return;
        };
        let descs = [
            EventDataDescriptor::from_bytes(
                &PROVIDER_METADATA,
                EVENT_DATA_DESCRIPTOR_TYPE_PROVIDER_METADATA,
            ),
            EventDataDescriptor::from_bytes(meta_slice, EVENT_DATA_DESCRIPTOR_TYPE_EVENT_METADATA),
            EventDataDescriptor::from_val(&v1),
            EventDataDescriptor::from_val(&v2),
        ];
        self.emit(handle, level, &descs);
    }

    fn write_u32_hex_event(
        &self,
        level: u8,
        event_name: &[u8],
        f1: &[u8],
        v1: u32,
        f2: &[u8],
        v2: u32,
    ) {
        let handle = self.reg_handle.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let mut meta = [0u8; 128];
        let Some(meta_len) = build_event_metadata(
            &mut meta,
            event_name,
            &[(f1, TLG_IN_UINT32), (f2, TLG_IN_HEXINT32)],
        ) else {
            return;
        };
        let Some(meta_slice) = meta.get(..meta_len) else {
            return;
        };
        let descs = [
            EventDataDescriptor::from_bytes(
                &PROVIDER_METADATA,
                EVENT_DATA_DESCRIPTOR_TYPE_PROVIDER_METADATA,
            ),
            EventDataDescriptor::from_bytes(meta_slice, EVENT_DATA_DESCRIPTOR_TYPE_EVENT_METADATA),
            EventDataDescriptor::from_val(&v1),
            EventDataDescriptor::from_val(&v2),
        ];
        self.emit(handle, level, &descs);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "ETW fixed-stack event descriptor helper avoids heap allocation"
    )]
    fn write_four_u32_event(
        &self,
        level: u8,
        event_name: &[u8],
        f1: &[u8],
        v1: u32,
        f2: &[u8],
        v2: u32,
        f3: &[u8],
        v3: u32,
        f4: &[u8],
        v4: u32,
    ) {
        let handle = self.reg_handle.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let mut meta = [0u8; 128];
        let Some(meta_len) = build_event_metadata(
            &mut meta,
            event_name,
            &[
                (f1, TLG_IN_UINT32),
                (f2, TLG_IN_UINT32),
                (f3, TLG_IN_UINT32),
                (f4, TLG_IN_UINT32),
            ],
        ) else {
            return;
        };
        let Some(meta_slice) = meta.get(..meta_len) else {
            return;
        };
        let descs = [
            EventDataDescriptor::from_bytes(
                &PROVIDER_METADATA,
                EVENT_DATA_DESCRIPTOR_TYPE_PROVIDER_METADATA,
            ),
            EventDataDescriptor::from_bytes(meta_slice, EVENT_DATA_DESCRIPTOR_TYPE_EVENT_METADATA),
            EventDataDescriptor::from_val(&v1),
            EventDataDescriptor::from_val(&v2),
            EventDataDescriptor::from_val(&v3),
            EventDataDescriptor::from_val(&v4),
        ];
        self.emit(handle, level, &descs);
    }

    fn write_gpu_event(
        &self,
        level: u8,
        event_name: &[u8],
        luid_low: u32,
        luid_high: i32,
        status: i32,
    ) {
        let handle = self.reg_handle.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let mut meta = [0u8; 128];
        let Some(meta_len) = build_event_metadata(
            &mut meta,
            event_name,
            &[
                (b"luid_low", TLG_IN_HEXINT32),
                (b"luid_high", TLG_IN_INT32),
                (b"status", TLG_IN_HEXINT32),
            ],
        ) else {
            return;
        };
        let Some(meta_slice) = meta.get(..meta_len) else {
            return;
        };
        let status_u32 = status as u32;
        let descs = [
            EventDataDescriptor::from_bytes(
                &PROVIDER_METADATA,
                EVENT_DATA_DESCRIPTOR_TYPE_PROVIDER_METADATA,
            ),
            EventDataDescriptor::from_bytes(meta_slice, EVENT_DATA_DESCRIPTOR_TYPE_EVENT_METADATA),
            EventDataDescriptor::from_val(&luid_low),
            EventDataDescriptor::from_val(&luid_high),
            EventDataDescriptor::from_val(&status_u32),
        ];
        self.emit(handle, level, &descs);
    }

    fn write_ioctl_error_event(&self, ioctl_code: u32, status: i32, detail: &[u8]) {
        let handle = self.reg_handle.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let mut msg = [0u8; 64];
        let copy_len = detail.len().min(msg.len().saturating_sub(1));
        if let (Some(dst), Some(src)) = (msg.get_mut(..copy_len), detail.get(..copy_len)) {
            dst.copy_from_slice(src);
        }
        if let Some(nul_slot) = msg.get_mut(copy_len) {
            *nul_slot = 0;
        }

        let mut meta = [0u8; 128];
        let Some(meta_len) = build_event_metadata(
            &mut meta,
            b"IoctlError",
            &[
                (b"ioctl_code", TLG_IN_HEXINT32),
                (b"status", TLG_IN_HEXINT32),
                (b"detail", TLG_IN_ANSISTRING),
            ],
        ) else {
            return;
        };
        let Some(meta_slice) = meta.get(..meta_len) else {
            return;
        };
        let Some(msg_slice) = msg.get(..copy_len.saturating_add(1)) else {
            return;
        };
        let status_u32 = status as u32;
        let descs = [
            EventDataDescriptor::from_bytes(
                &PROVIDER_METADATA,
                EVENT_DATA_DESCRIPTOR_TYPE_PROVIDER_METADATA,
            ),
            EventDataDescriptor::from_bytes(meta_slice, EVENT_DATA_DESCRIPTOR_TYPE_EVENT_METADATA),
            EventDataDescriptor::from_val(&ioctl_code),
            EventDataDescriptor::from_val(&status_u32),
            EventDataDescriptor::from_bytes(msg_slice, 0),
        ];
        self.emit(handle, ETW_LEVEL_WARN, &descs);
    }

    /// Emits a 64-bit counter telemetry event.
    pub fn log_counter_u64(&self, event_name: &[u8], field_name: &[u8], value: u64) {
        let handle = self.reg_handle.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let mut meta = [0u8; 128];
        let Some(meta_len) =
            build_event_metadata(&mut meta, event_name, &[(field_name, TLG_IN_UINT64)])
        else {
            return;
        };
        let Some(meta_slice) = meta.get(..meta_len) else {
            return;
        };
        let descs = [
            EventDataDescriptor::from_bytes(
                &PROVIDER_METADATA,
                EVENT_DATA_DESCRIPTOR_TYPE_PROVIDER_METADATA,
            ),
            EventDataDescriptor::from_bytes(meta_slice, EVENT_DATA_DESCRIPTOR_TYPE_EVENT_METADATA),
            EventDataDescriptor::from_val(&value),
        ];
        self.emit(handle, ETW_LEVEL_INFO, &descs);
    }

    fn emit(&self, handle: u64, level: u8, descs: &[EventDataDescriptor]) {
        let descriptor = EventDescriptor {
            id: 0,
            version: 0,
            channel: 11,
            level,
            opcode: 0,
            task: 0,
            keyword: 1,
        };
        // SAFETY: `handle` is registered; `descriptor` and `descs` point to initialized memory.
        unsafe {
            let _ = EventWriteTransfer(
                handle,
                &descriptor,
                ptr::null(),
                ptr::null(),
                descs.len() as u32,
                descs.as_ptr(),
            );
        }
    }
}

impl Default for EtwLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EtwLogger {
    fn drop(&mut self) {
        let handle = self.reg_handle.swap(0, Ordering::AcqRel);
        if handle != 0 {
            // SAFETY: `handle` was returned by `EventRegister` and has not been unregistered.
            unsafe {
                let _ = EventUnregister(handle);
            }
        }
    }
}

fn push_bytes(buf: &mut [u8], cursor: &mut usize, bytes: &[u8]) -> Option<()> {
    let end = cursor.checked_add(bytes.len())?;
    let dst = buf.get_mut(*cursor..end)?;
    dst.copy_from_slice(bytes);
    *cursor = end;
    Some(())
}

fn push_u8(buf: &mut [u8], cursor: &mut usize, val: u8) -> Option<()> {
    let slot = buf.get_mut(*cursor)?;
    *slot = val;
    *cursor = cursor.checked_add(1)?;
    Some(())
}

fn build_event_metadata(
    buf: &mut [u8],
    event_name: &[u8],
    fields: &[(&[u8], u8)],
) -> Option<usize> {
    let mut cursor: usize = 3;
    push_bytes(buf, &mut cursor, event_name)?;
    push_u8(buf, &mut cursor, 0)?;
    for &(field_name, in_type) in fields {
        push_bytes(buf, &mut cursor, field_name)?;
        push_u8(buf, &mut cursor, 0)?;
        push_u8(buf, &mut cursor, in_type)?;
    }
    let total_u16 = u16::try_from(cursor).ok()?;
    let [s0, s1] = total_u16.to_le_bytes();
    let header = buf.get_mut(..3)?;
    if let [b0, b1, b2] = header {
        *b0 = s0;
        *b1 = s1;
        *b2 = 0;
    } else {
        return None;
    }
    Some(cursor)
}
