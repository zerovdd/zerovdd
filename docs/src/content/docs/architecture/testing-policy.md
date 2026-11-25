---
title: Testing Policy
description: Three-tier verification strategy spanning portable host tests, WDK Universal Driver compliance, and bare-metal Windows 11 hardware-in-the-loop checks.
---

ZeroVDD separates deterministic host tests, Windows WDK/build validation, and bare-metal Windows 11 hardware-in-the-loop (HIL) checks into three verification tiers.

## Verification Tiers

### Tier 1: Fast Local & Portable Gate (`mise run verify:agent`)

Runs on Windows, Linux, and macOS with zero WDK and zero elevated privileges:

- `mise run fmt:check`: `cargo fmt --all -- --check` (root + `driver-wdk`)
- `mise run lint`: `cargo clippy --workspace --all-targets -- -D warnings`
- `mise run check:win`: `cargo check --target x86_64-pc-windows-msvc` across Windows user-mode crates
- `mise run check:nostd`: `cargo check --no-default-features` across `#![no_std]` Layer 0/1 crates
- `mise run lints:static`: compiled static repository lints in `crates/zerovdd-checks` (DAG, `NoPanic`, `// SAFETY:`, doc-comments, CI SHA pins, layout/EOL, WCAG contrast, zero plan codes, source-text test boundary, zero test sleep)
- `mise run test:scripts`: `.githooks` and shell script tests in isolated temporary git repositories
- `mise run test`: unit, property (`proptest`), simulation (`FakeIddPlatform`), and regression corpus tests across the root workspace
- `mise run machete`, `mise run deny:rust`, `mise run check:emdash`, `mise run check:drift`

### Tier 2: Windows WDK & Compliance Validation (`mise run driver:build` + CI)

Runs on Windows 11 with Visual Studio Build Tools and Windows 11 WDK:

- Compiles `driver-wdk` (`zerovdd.dll`) for `x64` and `ARM64`
- Runs `StampInf`, `InfVerif /u`, `InfVerif /h`, `InfVerif /w` (0 errors), `ApiValidator` (`All binaries are Universal`), and `Inf2Cat` (0 errors/warnings)
- Builds and tests Windows user-mode crates natively (`x64` and `ARM64`)
- Runs `cargo-audit` against `.cargo/audit.toml` and `mise run docs:build` for the Starlight documentation site

### Tier 3: Hardware-in-the-Loop (HIL) Verification on Windows 11

Runs on live Windows 11 22H2+ / 24H2 hardware:

- MSI clean install, same-version upgrade, clean uninstall, `DIRID 13` verification, and `zerovdd doctor --capture-etw`
- Non-elevated Medium-integrity IPC parity (`display add/list/set-mode/switch-gpu/remove/restore`) alongside `ERROR_ACCESS_DENIED` on direct `\ZeroVddControl` open
- Portrait rotation (`90`/`270`), negative coordinates, `--ppi` scaling, HDR10 DCI-P3 / 10 bpc, custom `--edid`, and `zerovdd edid inspect`
- Native `zerovdd-tray.exe` `--check-only`, single-instance mutex, and ~0 ms idle CPU verification
- Sunshine `configure --apply`, simulated `sunshine.exe` force-kill crash recovery, and `scripts/windows/stress-test.ps1` soak + raw IOCTL fuzzing

## Mock vs. Real Hardware Coverage

| Subsystem | Tier 1 (Pure Rust / Mock) | Tier 2 (WDK / Build) | Tier 3 (Live Windows HIL) |
| :--- | :--- | :--- | :--- |
| IOCTL ABI & IPC framing | Unit + proptest + fuzz corpus | Native Windows compile | Raw IOCTL fuzz + ACL denial |
| EDID 1.4 + CTA-861-G + HDR10 | Checksum, PPI, bpc, chromaticity | Linked into `zerovdd.dll` | Windows EDID parse, DPI, HDR |
| CVT-RBv2 64-bit timing math | 8K@240Hz, fractional Hz | Linked into `zerovdd.dll` | `EnumDisplaySettings` parity |
| Driver state machine | `FakeIddPlatform` re-entrancy & soak | Linked into `zerovdd.dll` | Live `WUDFHost.exe` soak + TDR |
| CCD & Win11 24H2 HDR/ACM | Captured topology fixtures | Native `windows` compile | Live multi-monitor, portrait & HDR |
| Session-0 service & tray | State machine + pipe/model tests | Native service + tray build | Crash recovery, Lock Screen, tray idle |
| INF & UMDF compliance | `crate_dag_lint`, `driver_panic_policy_lint` | `InfVerif /u /h /w`, `ApiValidator` | Clean MSI install/upgrade/uninstall |

## Test Authoring Rules

- **Zero `sleep` in tests:** Enforced by `test_sleep_lint`. Synchronize with barriers, channels, or injected clocks.
- **Negative fixtures required:** Every static lint and validator test includes known-bad fixtures proving invalid inputs fail.
- **No host state mutation in Tier 1:** Unit and script tests run in isolated `TempDir` directories and pass `ZEROVDD_NO_SERVICE_IPC=1` for offline CLI validation tests.
