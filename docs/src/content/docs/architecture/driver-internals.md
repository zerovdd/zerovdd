---
title: Driver Internals
description: UMDF 2.33 / IddCx 1.10 design of zerovdd.dll including DIRID 13 INF, Control Device isolation, Pending-to-Active re-entrancy registry, device-scoped D3D11, and zero-polling swapchains.
---

`zerovdd.dll` is a `#![no_std]` + `alloc` UMDF 2.33 / IddCx 1.10 Indirect Display Driver engineered to pass Universal Driver (`InfVerif /u /h /w`, `ApiValidator`) checks with zero panics and zero idle wakeups.

## Universal `DIRID 13` INF & `Class=Display` Coexistence

`driver-wdk/zerovdd.inf` targets Windows 11 22H2+ (`NTamd64.10.0...22621` / `NTarm64.10.0...22621`) with:

- `Class = Display`, `ClassVer = 2.0`, `PnpLockdown = 1`
- `DestinationDirs = 13` (Driver Store isolation; zero files copied into `System32\drivers`)
- `UpperFilters = IndirectKmd`
- `UmdfLibraryVersion = 2.33.0`, `UmdfExtensions = IddCx0102`

The INF intentionally omits restrictive `HKR,,Security` overrides on the `Class=Display` PnP FDO so DWM, DXGI, and non-admin games can open the display adapter normally.

## DeviceInterface + `\ZeroVddControl` Security Gate

In UMDF 2.33, `WdfControlDeviceInitAllocate` is KMDF-only. Rather than weakening `Class=Display` ACLs or granting `Everyone` (`WD`) access to privileged IOCTLs, `zerovdd-driver` isolates control traffic using a dedicated WDF DeviceInterface with a `ReferenceString`:

1. `evt_driver_device_add` calls `WdfDeviceCreateDeviceInterface` with `ZEROVDD_CONTROL_INTERFACE_GUID` (`{2963AE53-4CF3-4385-929A-A8348595217E}`) and `ReferenceString = "ZeroVddControl"` (no bare DOS symbolic link, because a bare symlink delivers an empty `FileObject->FileName`).
2. `WdfDeviceInitSetFileObjectConfig` attaches per-handle `FileContext { is_control, authorized, cleaned_up, in_flight_ioctls }` with `evt_device_file_create`, `evt_file_cleanup`, and `evt_file_close`.
3. On open:
   - Non-control opens (empty filename from Display/DXGI/DWM) record `is_control = false; authorized = false` and return `STATUS_SUCCESS`.
   - Control opens (`\ZeroVddControl`) verify `KernelMode` or impersonate the caller (`WdfRequestImpersonate(SecurityImpersonation)`) and check membership in `NT AUTHORITY\SYSTEM` or `BUILTIN\Administrators`. Unprivileged callers fail with `STATUS_ACCESS_DENIED`.
4. `evt_iddcx_device_io_control` validates `is_control && authorized && !cleaned_up` before dispatching to `zerovdd-engine`.
5. `evt_file_cleanup` removes every handle-bound monitor owned by that handle's `FileKey`, preventing ghost monitors if a client process crashes without calling `IOCTL_REMOVE_DISPLAY`.

## Re-Entrancy-Safe `Pending -> Active` Monitor Registry

`IddCxMonitorArrival` synchronously invokes `EvtIddCxParseMonitorDescription2` on the calling thread before returning. Holding a registry mutex across `IddCxMonitorArrival` would deadlock. `zerovdd-engine` eliminates lock inversion with a two-phase slot protocol:

1. Under lock, allocate a free connector slot and insert `SlotPhase::Pending` holding `Arc<MonitorSpec>` (synthesized or validated custom EDID + precomputed CVT-RBv2 modes).
2. Drop the lock, call `IddCxMonitorCreate`, then call `IddCxMonitorArrival`.
3. Synchronous `ParseMonitorDescription2` and `MonitorQueryTargetModes2` callbacks acquire the lock briefly, clone `Arc<MonitorSpec>` (matching `Pending` or `Active`), and release the lock immediately.
4. Once `IddCxMonitorArrival` returns, re-acquire the lock:
   - On success, promote `Pending -> Active { adapter_luid, target_id }`.
   - On failure, atomically roll back the slot and delete the entry so zero ghost monitors or leaked connector slots remain.

The registry also guarantees idempotent `MonitorGuid` reconnects, D0/D3 `AdapterState` idempotency (`IddCxAdapterInitAsync` executes once across sleep/resume), and a `PendingGpuSwitch` circuit breaker (`max_attempts`) preventing infinite abandon loops on invalid render adapter LUIDs.

## Device-Scoped D3D11, Zero-Polling Swapchains & HW Cursor

- **Device-scoped `D3D11DeviceManager` (`d3d.rs`):** Lives inside the WDF device context and drops in `EvtDeviceReleaseHardware` (never inside a `static` global destroyed under Loader Lock during `DLL_PROCESS_DETACH`). Caches `ID3D11Device` per adapter LUID, skips WARP software adapters, validates `GetDeviceRemovedReason() == S_OK` and `IDXGIFactory1::IsCurrent()`, and recovers cleanly on TDR (`DXGI_ERROR_DEVICE_REMOVED`, `DXGI_ERROR_DEVICE_RESET`, `DXGI_ERROR_ACCESS_LOST`).
- **Zero-polling `SwapChainWorker` (`swapchain.rs`):** Each assigned swapchain runs an MMCSS `"DisplayPostProcessing"` worker thread waiting on `WaitForMultipleObjects([terminate_event, available_buffer_event], INFINITE)` (zero idle polling wakeups; `terminate_event` at index `0` plus a pre-acquire non-blocking check ensures prompt exit under heavy load). An RAII `SwapChainGuard` executes `WdfObjectDelete(hSwapChain)` on every exit path, and `EvtIddCxMonitorUnassignSwapChain` joins the thread synchronously before returning.
- **Unnamed hardware cursor (`cursor.rs`):** `HardwareCursorHandle` allocates an unnamed auto-reset Win32 event closed on `Drop`, eliminating handle leaks and cross-monitor name collisions, draining shapes via `IddCxMonitorQueryHardwareCursor3`/`2`/`1`.
- **Structured ETW TraceLogging (`etw.rs`):** Provider `ZeroVdd.Driver` (`{06714745-69DC-41FF-9684-C532D5183A74}`) emits non-blocking diagnostics without pipe or file-I/O deadlocks.
