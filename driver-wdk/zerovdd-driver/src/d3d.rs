//! Swapchain-scoped Direct3D 11 / DXGI device factory with deterministic flush and teardown.

use core::ffi::c_void;
use core::ptr;

use zerovdd_engine::AdapterLuid;
use zerovdd_iddcx_sys::{
    zerovdd_com_release, zerovdd_d3d_create_device_for_luid, zerovdd_d3d_flush_device, HResult,
};

/// `HRESULT` indicating the GPU was reset or removed (`DXGI_ERROR_DEVICE_REMOVED`).
pub const DXGI_ERROR_DEVICE_REMOVED: HResult = 0x887A_0005_u32 as i32;
/// `HRESULT` indicating the GPU device encountered a TDR reset (`DXGI_ERROR_DEVICE_RESET`).
pub const DXGI_ERROR_DEVICE_RESET: HResult = 0x887A_0007_u32 as i32;
/// `HRESULT` indicating DXGI desktop/device access was lost (`DXGI_ERROR_ACCESS_LOST`).
pub const DXGI_ERROR_ACCESS_LOST: HResult = 0x887A_0026_u32 as i32;

/// Returns `true` when `hr` indicates a GPU TDR, device removal, or lost device surface.
#[must_use]
pub const fn is_device_lost_hr(hr: HResult) -> bool {
    hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET || hr == DXGI_ERROR_ACCESS_LOST
}

/// Swapchain-scoped owner of `ID3D11Device`, `IDXGIDevice`, and `IDXGIFactory1` COM handles.
///
/// Flushes the immediate D3D11 context before releasing COM objects so deferred-destruction
/// shared swapchain resources and `DxgkSharedSyncObject` handles reclaim deterministically.
pub struct DxgiDeviceGuard {
    d3d11_device: *mut c_void,
    dxgi_device: *mut c_void,
    dxgi_factory: *mut c_void,
}

// SAFETY: `ID3D11Device`, `IDXGIDevice`, and `IDXGIFactory1` are free-threaded COM interfaces
// owned exclusively by a single `SwapChainWorker` thread and flushed/released on `Drop`.
unsafe impl Send for DxgiDeviceGuard {}

impl DxgiDeviceGuard {
    /// Returns the raw `IDXGIDevice*` pointer for `IddCxSwapChainSetDevice`.
    #[must_use]
    pub fn as_ptr(&self) -> *mut c_void {
        self.dxgi_device
    }
}

impl Drop for DxgiDeviceGuard {
    fn drop(&mut self) {
        if !self.d3d11_device.is_null() {
            // SAFETY: `self.d3d11_device` is a live `ID3D11Device` whose `ClearState`/`Flush`
            // drains deferred destruction of released swapchain surfaces and sync objects.
            unsafe {
                zerovdd_d3d_flush_device(self.d3d11_device);
            }
        }
        // SAFETY: Each non-null COM pointer holds an owned reference released once on drop.
        unsafe {
            zerovdd_com_release(self.dxgi_device);
            zerovdd_com_release(self.d3d11_device);
            zerovdd_com_release(self.dxgi_factory);
        }
        self.dxgi_device = ptr::null_mut();
        self.d3d11_device = ptr::null_mut();
        self.dxgi_factory = ptr::null_mut();
    }
}

/// Device-scoped Direct3D 11 / DXGI factory creating swapchain-owned devices without globals.
#[derive(Debug, Default, Clone, Copy)]
pub struct D3D11DeviceManager;

impl D3D11DeviceManager {
    /// Creates a stateless [`D3D11DeviceManager`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Creates a swapchain-scoped `ID3D11Device` + `IDXGIDevice` + `IDXGIFactory1` for `luid`.
    ///
    /// Skips software WARP adapters and drops COM handles cleanly when the swapchain ends,
    /// avoiding immortal WDDM/UMD handle accumulation across monitor hotplug cycles.
    pub fn create_device(&self, luid: AdapterLuid) -> Result<DxgiDeviceGuard, HResult> {
        let mut d3d11_device: *mut c_void = ptr::null_mut();
        let mut dxgi_device: *mut c_void = ptr::null_mut();
        let mut dxgi_factory: *mut c_void = ptr::null_mut();
        // SAFETY: Output pointers are valid locals; `zerovdd_d3d_create_device_for_luid`
        // skips `DXGI_ADAPTER_FLAG_SOFTWARE` and returns AddRef'd COM objects on `S_OK`.
        let hr = unsafe {
            zerovdd_d3d_create_device_for_luid(
                luid.low,
                luid.high,
                &mut d3d11_device,
                &mut dxgi_device,
                &mut dxgi_factory,
            )
        };
        if hr < 0 || d3d11_device.is_null() || dxgi_device.is_null() || dxgi_factory.is_null() {
            // SAFETY: Release any partial COM pointers if creation failed unexpectedly.
            unsafe {
                zerovdd_com_release(dxgi_device);
                zerovdd_com_release(d3d11_device);
                zerovdd_com_release(dxgi_factory);
            }
            return Err(if hr < 0 { hr } else { -2147467259_i32 });
        }

        Ok(DxgiDeviceGuard {
            d3d11_device,
            dxgi_device,
            dxgi_factory,
        })
    }
}
