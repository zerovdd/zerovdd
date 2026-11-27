# Changelog

All notable changes to `ZeroVDD` (`zerovdd`) are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Windows 11 UMDF 2 / IddCx 1.10 Virtual Display Driver (`zerovdd.dll`, Universal `DIRID 13` `zerovdd.inf`, `x64` & `ARM64`), WDF Control Device interface (`\ZeroVddControl` gated to `SYSTEM` + `Administrators`), re-entrancy-safe `Pending -> Active` monitor registry, device-scoped D3D11 + zero-polling MMCSS swapchain workers, unnamed hardware cursor handler, and ETW TraceLogging (`ZeroVdd.Driver`).
- `zerovdd-edid` and `zerovdd-modes` (`#![no_std]`): EDID 1.4 + CTA-861-G synthesizer, custom EDID validation (`128..=512` bytes), explicit `--color-space` (`bt709`, `bt2020`, `dci-p3`), `--bpc` (`8`, `10`, `12` bpc), configurable `--ppi` (`48..=600`), per-display HDR10 mastering luminance overrides (`--max-nits`, `--max-fall`, `--min-nits-10k`), and 64-bit VESA CVT-RBv2 mode planning (up to 8K@240Hz and fractional 59.94/119.88/239.76 Hz).
- Managed Session-0 Windows service (`zerovdd-svc.exe`) with local-user ACL'd named pipe (`\\.\pipe\zerovdd-svc`, `SY`/`BA`/`IU`/`BU` + `PIPE_REJECT_REMOTE_CLIENTS`), active console-session CCD trampoline (supporting logged-in desktop, UAC, Lock Screen `Win+L`, Windows 11 CCD ghost-path filtering, and reboot recovery), persistent display reconciliation, default-on dynamic PnP `headless_standby` (4 s attach / 500 ms depart debounce, suspend safety, and TDR self-healing), and `sunshine.exe` crash supervision.
- Full CLI (`zerovdd.exe`): `setup` (SetupAPI/`newdev` Staging + pinned dev-cert helper), `display` (`add`, `remove`, `list`, `set-mode`, `switch-gpu`, `restore`, `--orientation`, `--position`, `--primary`, `--edid`), `edid inspect`, `sunshine` (`start`, `stop`, `status`, `configure` with `--no-headless-standby`), and `doctor` (`--json`, `--capture-etw`, with non-elevated IPC fallback for active display listing).
- Native Win32 system tray companion (`zerovdd-tray.exe`) with single-instance mutex, `TaskbarCreated` recovery, lazy menu snapshots, quick-add presets, per-display HDR/resolution switching, GPU switching, Sunshine status/stop/configure, physical display restore, `config.toml` opener, doctor JSON clipboard export, and Windows startup toggle.
- WiX v5 MSI installer (`installer/zerovdd.wxs`), SignPath Foundation three-stage release pipeline (`.github/workflows/release.yml`) with Partner Center attestation CAB + CycloneDX SBOM generation, WinGet (`ZeroVDD.ZeroVDD`) templates, and Scoop (`zerovdd.json`) manifest.
