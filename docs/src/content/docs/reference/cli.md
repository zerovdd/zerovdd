---
title: CLI Reference
description: Complete command, subcommand, flag, selector, and JSON output contract for zerovdd.exe, zerovdd-svc.exe, and zerovdd-tray.exe.
---

All `zerovdd` subcommands exit `0` on success and non-zero with a diagnostic message on stderr on failure. Pass `--json` to any operational subcommand for structured JSON output on stdout.

## Top-Level Commands (`zerovdd.exe`)

| Command | Purpose | Elevation Required |
| :--- | :--- | :--- |
| `zerovdd setup` | Install, upgrade, uninstall, or inspect the driver package and `Root\ZeroVdd` node. | Yes (except `status`) |
| `zerovdd display` | Add, update, list, remove displays, switch render GPU, or restore physical topology. | No when `zerovdd-svc` is running; Yes standalone |
| `zerovdd edid` | Validate and inspect raw EDID binary files (`128..=512` bytes). | No |
| `zerovdd sunshine` | Manage Sunshine sessions (`start`, `stop`, `status`) and update `sunshine.conf`. | `configure --apply` Yes; `start/stop/status` No |
| `zerovdd doctor` | Emit driver, `WUDFHost.exe`, GPU, OS HDR, Control Device, and service diagnostics. | Optional (Yes for Control Device, modules, ETW) |

---

## `zerovdd setup`

### `zerovdd setup install` / `zerovdd setup upgrade`

Stages `zerovdd.inf`, `zerovdd.dll`, and `zerovdd.cat` into the Driver Store (`DIRID 13`) and creates or updates `Root\ZeroVdd` without duplicating device nodes.

| Flag | Value | Description |
| :--- | :--- | :--- |
| `--inf` | `<PATH>` | Path to `zerovdd.inf` (defaults to executable-relative or `dist/<arch>/zerovdd.inf`). |
| `--dev-cert` | `<PATH>` | Optional local development `.cer` / `.pem` certificate to import into `LocalMachine\Root` and `TrustedPublisher`. |
| `--expected-sha256` | `<HEX>` | Mandatory 64-character SHA-256 hex digest when `--dev-cert` is supplied. |
| `--json` | flag | Emit install/upgrade summary as JSON. |

### `zerovdd setup uninstall`

Stops `zerovdd-svc`, removes all `Root\ZeroVdd` device nodes, uninstalls staged `oem*.inf` Driver Store packages, and removes tracked development certificates recorded in `%ProgramData%\zerovdd\install-state.json`. Supports `--json`.

### `zerovdd setup status`

Reports Driver Store INF staging, `DIRID 13` isolation, active `Root\ZeroVdd` device nodes, and tracked dev certs. Supports `--json`.

---

## `zerovdd display`

### `zerovdd display add <WxH@Hz>`

Creates or idempotently reuses a virtual display. Positional `<WxH@Hz>` accepts integer or fractional refresh rates (for example `3840x2160@59.94` or `2560x1600@90`).

| Flag | Value | Description |
| :--- | :--- | :--- |
| `--name` | `<NAME>` | Logical client/display label used for EDID naming and deterministic UUIDv5 GUID derivation. |
| `--guid` | `<UUID>` | Explicit monitor container UUID (overrides UUID derived from `--name`). |
| `--gpu` | `<SELECTOR>` | Render GPU selector (`auto`, `luid:<low>:<high>`, `pci:<bus>:<dev>.<fn>`, or friendly name substring). |
| `--hdr` | flag | Enable HDR10 (SMPTE ST 2084 PQ) profile and Windows HDR state. |
| `--color-space` | `auto`, `bt709`, `bt2020`, `dci-p3` | Explicit color gamut (`bt2020`/`dci-p3` require `--hdr`; `bt709` requires SDR). |
| `--bpc` | `auto`, `8`, `10`, `12` | Explicit per-component bit-depth cap (`8` requires SDR). |
| `--ppi` | `<48..=600>` | Explicit target pixel density in PPI for synthesized EDID physical dimensions. |
| `--max-nits` | `<100..=10000>` | Peak HDR10 mastering luminance override in nits. |
| `--max-fall` | `<NITS>` | Maximum frame-average light level override in nits. |
| `--min-nits-10k` | `<VAL>` | Minimum mastering luminance in `0.0001`-nit units (`500` = `0.05` nits). |
| `--edid` | `<PATH>` | Raw custom EDID binary file (`128..=512` bytes, multiple of 128). |
| `--extend` | flag | Keep physical monitors active and extend desktop with the virtual display. |
| `--isolated` | flag | Snapshot and disable physical monitors, setting the virtual display as sole primary at `(0, 0)`. |
| `--orientation` | `0`, `90`, `180`, `270` | Clockwise display orientation (`90`/`270` automatically transpose CCD source dimensions). |
| `--position` | `<X,Y>` | Explicit desktop origin when extending (supports negative coordinates like `-1080,0`). |
| `--primary` | flag | Promote the virtual display to primary `(0, 0)` even when using `--extend`. |
| `--hold` | flag | Standalone mode: keep Control Device handle open until `Ctrl+C` / termination. |
| `--lease` | `<SECS>` | Create a leased display persisting for `<SECS>` seconds (standalone or service TTL). |
| `--json` | flag | Emit result as JSON. |

### `zerovdd display set-mode <WxH@Hz>`

Updates resolution, refresh rate, HDR toggle, gamut, bit-depth cap, or HDR10 mastering luminance on an active virtual display in place.

| Flag | Value | Description |
| :--- | :--- | :--- |
| `--name` | `<NAME>` | Select active display by logical name. |
| `--guid` | `<UUID>` | Select active display by container UUID. |
| `--target-id` | `<ID>` | Select active display by non-zero OS VidPN target ID. |
| `--hdr` / `--sdr` | flag | Switch HDR10 on or off (omitting both preserves current HDR state). |
| `--color-space`, `--bpc`, `--max-nits`, `--max-fall`, `--min-nits-10k` | same as `add` | Optional explicit color and mastering overrides. |
| `--json` | flag | Emit update summary as JSON. |

### `zerovdd display remove`

Removes an active virtual display. Requires either `--guid <UUID>` or `--target-id <ID>`. Supports `--json`.

### `zerovdd display list`

Lists all active virtual displays (`monitor_guid`, `client_name`, `target_id`, `connector_index`, active mode, HDR state, ownership mode, and remaining lease). Supports `--json`.

### `zerovdd display switch-gpu <SELECTOR>`

Switches the driver's active render GPU adapter at runtime (`auto`, `luid:<low>:<high>`, `pci:<bus>:<dev>.<fn>`, or friendly name substring). Supports `--json`.

### `zerovdd display restore`

Stops any active session, departs all active virtual displays, and restores physical monitors from `%ProgramData%\zerovdd\topology-backup.json` (falling back to enabling all attached physical paths if no backup exists). Supports `--json`.

---

## `zerovdd edid`

### `zerovdd edid inspect <PATH>`

Validates a raw binary EDID file (`128..=512` bytes, multiple of 128) and reports block count, checksum status, EDID version/revision, PNP manufacturer, product name, physical size (`cm` and DTD `mm`), preferred timing, chromaticity, sRGB flag, and CTA-861-G video/colorimetry/HDR blocks. Supports `--json`.

---

## `zerovdd sunshine`

### `zerovdd sunshine configure`

Audits or updates LizardByte Sunshine's `sunshine.conf`. Runs in dry-run preview mode unless `--apply` is passed.

| Flag | Value | Description |
| :--- | :--- | :--- |
| `--config` | `<PATH>` | Override path to `sunshine.conf`. |
| `--apply` | flag | Write timestamped backup, apply idempotent `global_prep_cmd` and `dd_configuration_option = disabled` edits, and ensure `headless_standby = true` (the default) unless `--no-headless-standby` is passed. |
| `--clear-output-name` | flag | Remove pinned physical `output_name` entry so Sunshine captures the active primary display. |
| `--no-headless-standby` | flag | Persist `headless_standby = false` in `%ProgramData%\zerovdd\config.toml` to disable automatic headless standby. |
| `--json` | flag | Emit plan or applied diff as JSON. |

### `zerovdd sunshine start`

Starts a managed streaming session using `SUNSHINE_CLIENT_*` environment variables (overridable via `--width`, `--height`, `--fps`, `--hdr [true|false]`, `--client-id`, and `--owner-pid`). Supports `--json`.

### `zerovdd sunshine stop`

Tears down the active streaming session and restores physical display topology. Accepts `--client-id <ID>`, `--force` (bypass `owner_pid` check), and `--json`.

### `zerovdd sunshine status`

Reports active Sunshine session details and `zerovdd-svc` health telemetry. Supports `--json`.

---

## `zerovdd doctor`

Diagnoses driver staging, `Root\ZeroVdd` nodes, `WUDFHost.exe` memory/handles/modules, OS build and HDR/ACM API profile, DXGI GPUs, `\ZeroVddControl` health, active displays, and `zerovdd-svc` SCM + pipe health.

| Flag | Value | Description |
| :--- | :--- | :--- |
| `--capture-etw` | `<SECS>` | Run an elevated `logman` ETW capture for `ZeroVdd.Driver` into `%TEMP%\zerovdd-driver.etl` for `<SECS>` seconds. |
| `--json` | flag | Emit full structured diagnostic JSON. |

---

## Companion Binaries

- **`zerovdd-svc.exe --console`**: Run the service interactively in an elevated foreground console (stop the Windows service first with `sc stop zerovdd-svc`; press `Ctrl+C` for graceful teardown).
- **`zerovdd-tray.exe [--check-only] [--debug]`**: Launch the native Win32 notification-area companion. `--check-only` queries `\\.\pipe\zerovdd-svc`, builds the menu model, prints JSON summary, and exits `0`.
