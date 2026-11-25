//! OneCore-clean heap global allocator for `alloc` inside `WUDFHost.exe`.

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::c_void;
use core::mem::size_of;
use core::ptr;

use wdk_sys::windows::{GetProcessHeap, HeapAlloc, HeapFree, HeapReAlloc};
use wdk_sys::{BOOL, HANDLE, LPVOID, SIZE_T};

// WHY: 64-bit Windows `HeapAlloc` guarantees 16-byte `MEMORY_ALLOCATION_ALIGNMENT`.
const HEAP_GUARANTEED_ALIGN: usize = 16;

/// Global allocator backed by the Win32 process heap with over-alignment support.
pub struct ProcessHeapAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: ProcessHeapAllocator = ProcessHeapAllocator;

// SAFETY: `heap` must be a valid process heap handle; `layout.align()` must exceed
// `HEAP_GUARANTEED_ALIGN` and be a power of two.
unsafe fn alloc_overaligned(heap: HANDLE, layout: Layout) -> *mut u8 {
    let align = layout.align();
    let header = size_of::<usize>();
    let Some(padded) = layout.size().max(1).checked_add(align - 1) else {
        return ptr::null_mut();
    };
    let Some(total) = padded.checked_add(header) else {
        return ptr::null_mut();
    };

    // SAFETY: `heap` is valid and `total > 0`.
    let raw = unsafe { HeapAlloc(heap, 0, total as SIZE_T) };
    if raw.is_null() {
        return ptr::null_mut();
    }

    let base = raw as usize;
    let Some(min_addr) = base.checked_add(header) else {
        // SAFETY: `raw` was just returned by `HeapAlloc` on `heap`.
        unsafe {
            let _freed: BOOL = HeapFree(heap, 0, raw);
        }
        return ptr::null_mut();
    };
    let Some(rounded_up) = min_addr.checked_add(align - 1) else {
        // SAFETY: `raw` was just returned by `HeapAlloc` on `heap`.
        unsafe {
            let _freed: BOOL = HeapFree(heap, 0, raw);
        }
        return ptr::null_mut();
    };
    let aligned = rounded_up & !(align - 1);
    let slot = (aligned - header) as *mut usize;
    // SAFETY: `slot` lies within `[base, base + total - align]` ahead of `aligned`.
    unsafe {
        slot.write(base);
    }
    aligned as *mut u8
}

// SAFETY: `ptr` must have been returned by `alloc_overaligned` and remain non-null.
unsafe fn raw_base_from_overaligned(ptr: *mut u8) -> LPVOID {
    let header = size_of::<usize>();
    let slot = (ptr as usize - header) as *const usize;
    // SAFETY: `alloc_overaligned` stored the raw `HeapAlloc` pointer immediately before `ptr`.
    let base = unsafe { slot.read() };
    base as LPVOID
}

// SAFETY: Delegates directly to Win32 process heap APIs (`GetProcessHeap`, `HeapAlloc`,
// `HeapReAlloc`, `HeapFree`), preserving arbitrary alignment and returning null on OOM.
unsafe impl GlobalAlloc for ProcessHeapAllocator {
    // SAFETY: Caller must provide a valid non-zero layout per `GlobalAlloc::alloc` contract.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `GetProcessHeap` requires no preconditions.
        let heap: HANDLE = unsafe { GetProcessHeap() };
        if heap.is_null() {
            return ptr::null_mut();
        }
        if layout.align() <= HEAP_GUARANTEED_ALIGN {
            let size = layout.size().max(1) as SIZE_T;
            // SAFETY: `heap` is valid and `size > 0`.
            unsafe { HeapAlloc(heap, 0, size).cast::<u8>() }
        } else {
            // SAFETY: `heap` is valid and `layout.align() > HEAP_GUARANTEED_ALIGN`.
            unsafe { alloc_overaligned(heap, layout) }
        }
    }

    // SAFETY: Caller guarantees `ptr` came from `alloc` with matching `layout` (or is null).
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        // SAFETY: `GetProcessHeap` requires no preconditions.
        let heap: HANDLE = unsafe { GetProcessHeap() };
        if heap.is_null() {
            return;
        }
        let raw = if layout.align() <= HEAP_GUARANTEED_ALIGN {
            ptr.cast::<c_void>()
        } else {
            // SAFETY: `ptr` was produced by `alloc_overaligned`.
            unsafe { raw_base_from_overaligned(ptr) }
        };
        // SAFETY: `raw` is the original pointer returned by `HeapAlloc`.
        unsafe {
            let _freed: BOOL = HeapFree(heap, 0, raw);
        }
    }

    // SAFETY: Caller guarantees `ptr` came from `alloc` with `layout` and `new_size > 0`.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ptr.is_null() {
            return match Layout::from_size_align(new_size.max(1), layout.align()) {
                // SAFETY: Valid layout constructed via `Layout::from_size_align`.
                Ok(new_layout) => unsafe { self.alloc(new_layout) },
                Err(_) => ptr::null_mut(),
            };
        }
        if layout.align() <= HEAP_GUARANTEED_ALIGN {
            // SAFETY: `GetProcessHeap` requires no preconditions.
            let heap: HANDLE = unsafe { GetProcessHeap() };
            if heap.is_null() {
                return ptr::null_mut();
            }
            let size = new_size.max(1) as SIZE_T;
            // SAFETY: `ptr` was allocated directly by `HeapAlloc` on `heap`.
            let resized: LPVOID = unsafe { HeapReAlloc(heap, 0, ptr.cast::<c_void>(), size) };
            return resized.cast::<u8>();
        }

        let Ok(new_layout) = Layout::from_size_align(new_size.max(1), layout.align()) else {
            return ptr::null_mut();
        };
        // SAFETY: `new_layout` is valid and non-zero.
        let new_ptr = unsafe { self.alloc(new_layout) };
        if new_ptr.is_null() {
            return ptr::null_mut();
        }
        let copy_len = layout.size().min(new_size);
        // SAFETY: `ptr` is valid for `layout.size()` bytes and `new_ptr` for `new_size` bytes.
        unsafe {
            ptr::copy_nonoverlapping(ptr, new_ptr, copy_len);
            self.dealloc(ptr, layout);
        }
        new_ptr
    }
}
