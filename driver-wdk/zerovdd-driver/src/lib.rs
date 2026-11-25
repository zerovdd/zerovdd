//! UMDF 2.33 / IddCx 1.10 indirect display driver entrypoints and modules.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod allocator;
mod control_device;
mod cursor;
mod d3d;
mod etw;
mod iddcx;
mod security;
mod swapchain;
mod wdf;

#[cfg(not(test))]
use core::panic::PanicInfo;

use wdk_sys::{NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT};

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// UMDF 2 driver entrypoint invoked by `WUDFHost.exe` via `WdfDriverStubUm`.
///
/// # Safety
///
/// Must only be invoked by the UMDF 2 host with valid `driver` and `registry_path` pointers.
// SAFETY: Invoked by the UMDF 2 host with valid `driver` and `registry_path` pointers.
#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    // SAFETY: Caller guarantees valid UMDF `PDRIVER_OBJECT` and `PCUNICODE_STRING`.
    unsafe { wdf::wdf_driver_entry(driver, registry_path) }
}
