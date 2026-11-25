---
title: Configuration Reference
description: Complete schema, defaults, validation rules, and annotated example for %ProgramData%\zerovdd\config.toml.
---

`zerovdd-svc` and `zerovdd` read configuration from `%ProgramData%\zerovdd\config.toml`. The parser uses `#[serde(deny_unknown_fields)]` and strict range validation so typos or out-of-range settings fail fast rather than silently falling back.

## Complete Example

```toml title="C:\ProgramData\zerovdd\config.toml"
schema_version = 1
headless_standby = false

[gpu]
mode = "auto_best"
# Alternatives (replace `mode` above with one block):
# mode = "luid"
# low = 74561
# high = 0
#
# mode = "pci_bus"
# bus = 47
# device = 0
# function = 0
#
# mode = "friendly_name"
# name = "AMD Radeon RX 6800"

# Optional service-managed persistent displays (up to 16)
# [[persistent_displays]]
# name = "Workstation"
# width = 2560
# height = 1440
# refresh_mhz = 120000
# hdr = false
# color_space = "auto"      # "auto" | "bt709" | "bt2020" | "dci_p3"
# bit_depth = "auto"        # "auto" | "bpc8" | "bpc10" | "bpc12"
# target_ppi = 110          # 48..=600
# max_luminance_nits = 1000 # 100..=10000
# max_fall_nits = 400
# min_luminance_ten_thousandths = 500
# orientation = 0           # 0 | 90 | 180 | 270
# position = [2560, 0]
# primary = false
# edid_path = "C:\\ProgramData\\zerovdd\\edids\\custom.bin"

[hdr_defaults]
enabled = false
max_luminance_nits = 1000
max_fall_nits = 400
min_luminance_ten_thousandths = 500

[sunshine]
topology_mode = "isolate" # "isolate" | "extend"
default_width = 1920
default_height = 1080
default_refresh_mhz = 60000
default_hdr = false
restore_on_exit = true
# sunshine_config_path = "C:\\Program Files\\Sunshine\\config\\sunshine.conf"
# default_edid_path = "C:\\ProgramData\\zerovdd\\edids\\tv.bin"

[logging]
level = "info" # "error" | "warn" | "info" | "debug" | "trace"
etw_enabled = true
```

## Root Keys

| Key | Type | Default | Constraints / Purpose |
| :--- | :--- | :--- | :--- |
| `schema_version` | `u32` | `1` | Must equal `1`. Any other value is rejected. |
| `headless_standby` | `bool` | `false` | When `true`, attaches a `1920x1080@60` standby display if zero active displays exist at service startup or post-stream restore. |

## `[gpu]`

Tagged union keyed by `mode`:

| `mode` | Required Fields | Behavior |
| :--- | :--- | :--- |
| `"auto_best"` | *(none)* | Highest-VRAM non-software hardware GPU (default). |
| `"luid"` | `low: u32`, `high: i32` | Matches adapter by exact Windows `LUID`. |
| `"pci_bus"` | `bus: u32`, `device: u32`, `function: u32` | Matches adapter by PCI location. |
| `"friendly_name"` | `name: String` | Non-empty case-insensitive substring match on DXGI adapter description. |

## `[[persistent_displays]]`

Up to `16` displays kept active by `zerovdd-svc` whenever running.

| Key | Type | Default | Constraints |
| :--- | :--- | :--- | :--- |
| `name` | `String` | *(required)* | Unique non-empty label (`<= 63` UTF-8 bytes, no control chars). |
| `width` | `u32` | *(required)* | Active width in pixels (`320..=7680`). |
| `height` | `u32` | *(required)* | Active height in pixels (`200..=4320`). |
| `refresh_mhz` | `u32` | *(required)* | Refresh rate in **millihertz** (e.g. `60000` for `60 Hz`, `59940` for `59.94 Hz`, `119880` for `119.88 Hz`). |
| `hdr` | `bool` | `false` | Advertise and enable HDR10. |
| `color_space` | `String` | `"auto"` | `"auto"`, `"bt709"`, `"bt2020"`, or `"dci_p3"`. Must be compatible with `hdr`. |
| `bit_depth` | `String` | `"auto"` | `"auto"`, `"bpc8"`, `"bpc10"`, or `"bpc12"`. `"bpc8"` is SDR-only. |
| `target_ppi` | `Option<u16>` | `None` | Optional physical EDID pixel density (`48..=600`). |
| `max_luminance_nits` | `Option<u16>` | `None` | Peak HDR10 luminance (`100..=10000`). |
| `max_fall_nits` | `Option<u16>` | `None` | Max frame-average luminance (`1..=max_luminance_nits`). |
| `min_luminance_ten_thousandths` | `Option<u32>` | `None` | Min luminance in `0.0001`-nit units (`0..=max*10000`). |
| `orientation` | `Option<u16>` | `None` | Clockwise rotation: `0`, `90`, `180`, or `270`. |
| `position` | `Option<[i32; 2]>` | `None` | Explicit `[x, y]` desktop origin when extending. |
| `primary` | `bool` | `false` | Promote to primary `(0, 0)`. |
| `edid_path` | `Option<String>` | `None` | Non-empty path to raw `128..=512`-byte EDID binary. |

## `[hdr_defaults]`

| Key | Type | Default | Constraints |
| :--- | :--- | :--- | :--- |
| `enabled` | `bool` | `false` | Default HDR toggle when not overridden. |
| `max_luminance_nits` | `u16` | `1000` | `100..=10000`. |
| `max_fall_nits` | `u16` | `400` | `1..=max_luminance_nits`. |
| `min_luminance_ten_thousandths` | `u32` | `500` | `0..=max_luminance_nits * 10000` (`500` = `0.05` nits). |

## `[sunshine]`

| Key | Type | Default | Purpose |
| :--- | :--- | :--- | :--- |
| `topology_mode` | `String` | `"isolate"` | `"isolate"` (disable physical displays during stream) or `"extend"`. |
| `default_width` | `u32` | `1920` | Fallback width if `SUNSHINE_CLIENT_WIDTH` is unset. |
| `default_height` | `u32` | `1080` | Fallback height if `SUNSHINE_CLIENT_HEIGHT` is unset. |
| `default_refresh_mhz` | `u32` | `60000` | Fallback refresh in millihertz if `SUNSHINE_CLIENT_FPS` is unset. |
| `default_hdr` | `bool` | `false` | Fallback HDR flag if `SUNSHINE_CLIENT_HDR` is unset. |
| `restore_on_exit` | `bool` | `true` | Restore `topology-backup.json` on session stop or crash. |
| `sunshine_config_path` | `Option<String>` | `None` | Optional custom path to `sunshine.conf`. |
| `default_edid_path` | `Option<String>` | `None` | Optional custom EDID binary for Sunshine displays (falls back to synthesized EDID with a warning if invalid). |

## `[logging]`

| Key | Type | Default | Values |
| :--- | :--- | :--- | :--- |
| `level` | `String` | `"info"` | `"error"`, `"warn"`, `"info"`, `"debug"`, `"trace"`. |
| `etw_enabled` | `bool` | `true` | Emit structured ETW TraceLogging events. |
