---
title: Doctor and ETW Reference
description: Schema for zerovdd doctor --json telemetry and instructions for capturing and inspecting ZeroVdd.Driver ETW TraceLogging traces.
---

`zerovdd doctor` consolidates driver installation state, `WUDFHost.exe` process metrics, OS HDR capabilities, DXGI GPUs, `\ZeroVddControl` status, active virtual displays, and `zerovdd-svc` health into one report.

## `zerovdd doctor --json` Top-Level Schema

| Field | Type | Description |
| :--- | :--- | :--- |
| `cli_version` | `string` | Semantic version of `zerovdd.exe`. |
| `arch` | `string` | Target architecture (`x86_64` or `aarch64`). |
| `installer` | `object \| null` | Driver Store INF (`oem_inf_name`, `driver_store_path`, `dirid_13_isolated`, `driver_version`, `signer_name`), `device_nodes[]` (`instance_id`, `present`, `has_problem`, `problem_code`), and `tracked_dev_certs[]`. |
| `installer_error` | `string \| null` | Error message if SetupAPI queries failed. |
| `wudf_host_processes` | `array` | Running `WUDFHost.exe` processes (`pid`, `private_memory_bytes`, `working_set_bytes`, `handle_count`, `hosts_zerovdd_dll`). Note: `hosts_zerovdd_dll` requires elevation to inspect cross-process modules. |
| `os` | `object` | Real Windows version via `RtlGetVersion` (`major`, `minor`, `build`), `hdr_api_profile`, and `split_hdr_acm_supported` (`true` on build `>= 26100`). |
| `gpus` | `array` | Enumerated DXGI adapters (`name`, `adapter_luid_low`, `adapter_luid_high`, `vendor_id`, `device_id`, `pci_bus/device/function`, `dedicated_vram_bytes`, `is_software`). |
| `control_device` | `object` | Tagged state: `accessible` (`device_path`, `protocol_version`, `adapter_ready`, `hdr_capable`, `uptime_secs`, `last_gpu_switch_failed`), `access_denied`, or `not_installed`. |
| `active_displays` | `array` | Active virtual monitors returned by `IOCTL_LIST_DISPLAYS` when `control_device` is accessible. |
| `service` | `object` | Windows SCM `scm_state` (`running`, `stopped`, `not_installed`, etc.) and live `ipc_health` (`service_version`, `driver_connected`, `protocol_version`, `active_displays`, `uptime_secs`, `backup_present`). |
| `etw_capture` | `object \| null` | Present when `--capture-etw <SECS>` is passed (`provider_name`, `provider_guid`, `etl_path`, `duration_secs`, `captured`, `detail`). |

## ETW TraceLogging Provider (`ZeroVdd.Driver`)

`zerovdd.dll` emits structured, non-blocking Windows ETW TraceLogging events from inside `WUDFHost.exe` (avoiding named-pipe or file-logging deadlocks):

- **Provider Name:** `ZeroVdd.Driver`
- **Provider GUID:** `{06714745-69DC-41FF-9684-C532D5183A74}`

### Quick Capture via `zerovdd doctor`

From an **elevated Administrator** prompt:

```powershell
zerovdd doctor --capture-etw 10
```

This starts a temporary `logman` trace session, waits 10 seconds while you reproduce an event, stops the session, and writes `%TEMP%\zerovdd-driver.etl`.

### Manual Capture & Inspection (`logman` / `tracerpt` / WPA)

For longer captures or custom output paths:

```powershell
# Start ETW session
logman start ZeroVddTrace -p "{06714745-69DC-41FF-9684-C532D5183A74}" 0xffffffff 5 -o "$env:TEMP\zerovdd-manual.etl" -ets

# Reproduce display add/remove, HDR toggle, or GPU switch here...

# Stop ETW session
logman stop ZeroVddTrace -ets

# Decode events to XML/CSV summary or open in Windows Performance Analyzer (WPA)
tracerpt "$env:TEMP\zerovdd-manual.etl" -o "$env:TEMP\zerovdd-events.xml" -of XML -y
```
