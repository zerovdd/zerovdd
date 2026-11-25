---
title: IPC and Security Reference
description: Security boundaries, GUIDs, DACLs, IOCTL ABI v1.1 wire contracts, and named-pipe JSON IPC framing for ZeroVDD.
---

ZeroVDD separates privileged hardware control from everyday interactive display management using two distinct security boundaries: the UMDF Control Device interface (`\ZeroVddControl`) and the Session-0 service named pipe (`\\.\pipe\zerovdd-svc`).

## Stable Identifiers & GUIDs

| Identifier | Value | Scope |
| :--- | :--- | :--- |
| `ZEROVDD_CONTROL_INTERFACE_GUID` | `{2963AE53-4CF3-4385-929A-A8348595217E}` | WDF DeviceInterface class for `\ZeroVddControl` |
| ETW `ZeroVdd.Driver` Provider GUID | `{06714745-69DC-41FF-9684-C532D5183A74}` | Driver TraceLogging provider |
| `MONITOR_GUID_NAMESPACE` | `13AF2775-DDC4-4582-BDA0-F5D27EAEC25A` | UUIDv5 namespace for deterministic `MonitorContainerId` |
| WiX MSI `UpgradeCode` | `{E3B14E29-B75E-4BCC-8194-F71147EFD27B}` | Per-machine Windows Installer upgrade identity |
| PnP Hardware ID | `Root\ZeroVdd` | Root-enumerated UMDF 2 Display adapter node |
| EDID PNP Manufacturer ID | `ZVD` (`0x6AC4`) | Synthesized EDID 1.4 manufacturer tag |

## Control Device Interface (`\ZeroVddControl`)

- **Discovery:** `CM_Get_Device_Interface_ListW` for `{2963AE53-4CF3-4385-929A-A8348595217E}`, selecting the path ending in `\ZeroVddControl`.
- **Authorization Gate (`EvtDeviceFileCreate`):**
  - Handles opened with an empty filename (DWM, DXGI, games on `Class=Display`) succeed with `is_control = false; authorized = false`.
  - Handles opened against `\ZeroVddControl` require `KernelMode` or caller impersonation (`WdfRequestImpersonate(SecurityImpersonation)`) verifying membership in `NT AUTHORITY\SYSTEM` (`SECURITY_LOCAL_SYSTEM_RID`) or `BUILTIN\Administrators` (`DOMAIN_ALIAS_RID_ADMINS`). Non-elevated callers receive `STATUS_ACCESS_DENIED` (`ERROR_ACCESS_DENIED = 5`).
- **Handle-Bound Ownership (`EvtFileCleanup`):** Closing a Control Device handle (including client process crash or `taskkill /F`) immediately departs every handle-bound virtual display owned by that handle.

### IOCTL ABI (`v1.1.0`)

Protocol version `1.1.0` sets feature bit `PROTOCOL_FLAG_COLOR_AND_PPI = 1 << 3` and validates exact byte lengths on every `DeviceIoControl` request:

| IOCTL | Direction / Purpose |
| :--- | :--- |
| `IOCTL_GET_VERSION` | Returns `ProtocolVersion` `(major=1, minor=1, patch=0, flags)`. Handshake enforces `major == 1`. |
| `IOCTL_ADD_DISPLAY` | `AddDisplayRequest` (`656` bytes): GUID, client name, resolution, rational refresh, HDR profile, color space, bit-depth cap, `target_ppi`, HDR mastering metadata, optional custom EDID (`<= 512` bytes), ownership mode, lease TTL. |
| `IOCTL_REMOVE_DISPLAY` | Departs a display by `monitor_guid` or non-zero `target_id`. |
| `IOCTL_LIST_DISPLAYS` | Returns active monitor snapshots (`monitor_guid`, `client_name`, adapter LUID, `target_id`, active mode, HDR, lease TTL). |
| `IOCTL_SET_RENDER_ADAPTER` | Requests asynchronous render GPU adapter switch to target LUID (bounded by circuit breaker). |
| `IOCTL_RENEW_LEASE` | Extends TTL on an active leased virtual display. |
| `IOCTL_GET_STATUS` | Returns `adapter_ready`, `hdr_capable`, `active_displays`, `uptime_secs`, and `last_gpu_switch_failed`. |
| `IOCTL_UPDATE_DISPLAY_MODE` | `UpdateDisplayModeRequest` (`60` bytes): updates resolution, refresh, HDR profile, mastering luminance, color space, and bit-depth cap in place (`IddCxMonitorUpdateModes2` or re-arrival when EDID/color metadata changes). |

## Session-0 Service Named Pipe (`\\.\pipe\zerovdd-svc`)

- **Strict DACL (SDDL):** `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)`
  - `SY` (`LocalSystem`): Generic All
  - `BA` (`Builtin Administrators`): Generic All
  - `IU` (`Interactive Logged-On Users`): Generic Read + Generic Write
  - Never grants access to `Everyone` (`WD`) or `NETWORK`.
- **Wire Framing:** Length-prefixed (`u32` little-endian byte count, bounded) UTF-8 JSON `IpcEnvelope<T> { version: 1, request_id, payload }`.
- **Supported `IpcRequest` Variants:**
  - `StartSession`, `StopSession`, `GetSessionStatus`
  - `DisplayAdd`, `DisplayRemove`, `DisplaySetMode`, `DisplayList`
  - `ListGpus`, `SwitchGpu`, `RestoreTopology`, `GetHealth`
