# Testing Policy (`zerovdd`)

`zerovdd` separates deterministic host tests, Windows WDK/build validation, and bare-metal hardware-in-the-loop (HIL) checks into three clear verification tiers.

## Verification Tiers

### Tier 1: Fast Local & Portable Gate (`mise run verify:agent`)
Runs on Windows, Linux, and macOS with zero WDK and zero elevated privileges:
- `mise run fmt:check`: `cargo fmt --all -- --check`
- `mise run lint`: `cargo clippy --workspace --all-targets -- -D warnings`
- `mise run check:win`: `cargo check --target x86_64-pc-windows-msvc` across Windows user-mode crates
- `mise run check:nostd`: `cargo check --no-default-features` across `#![no_std]` Layer 0/1 crates
- `mise run lints:static`: compiled static repository lints in `crates/zerovdd-checks`
- `mise run test:scripts`: hook and script tests in isolated temporary git repositories
- `mise run test`: unit, property (`proptest`), simulation, and regression corpus tests across the root workspace
- `mise run machete`: unused dependency verification (`cargo-machete`)
- `mise run deny:rust`: license, advisory, ban, and source policy check (`cargo-deny`)
- `scripts/check-emdash-diff.sh`: diff-scoped em-dash check

### Tier 2: Windows WDK & Compliance Validation (`mise run driver:build` + CI)
Runs on Windows 11 with Visual Studio Build Tools and Windows 11 WDK (locally and in CI):
- Builds `driver-wdk` for `x64` and `ARM64`
- Runs `StampInf`, `InfVerif /u /h` (Universal Driver compliance, 0 errors), `ApiValidator` (0 unsupported APIs), and `Inf2Cat`
- Compiles and tests Windows user-mode crates natively (`x64` and `ARM64`)
- Runs `cargo-audit` against `.cargo/audit.toml`

### Tier 3: Hardware-in-the-Loop (HIL) Verification on Windows 11
Runs on a live Windows 11 22H2+ / 24H2 machine with Secure Boot enabled:
- Driver installation via `zerovdd setup install`, Driver Store (`DIRID 13`) verification, and `zerovdd doctor`
- 100-cycle and 500-cycle virtual display add/remove soak tests verifying flat `WUDFHost.exe` handle counts and private working set
- Mode, fractional refresh rate (59.94 / 119.88 / 239.76 Hz), default DPI (~100-150%), Win11 24H2 HDR/ACM toggle, and hardware cursor rendering verification
- Security verification: non-admin DXGI works normally while non-admin open of `\\.\ZeroVddControl` returns `ERROR_ACCESS_DENIED`
- Fault resilience: sleep/resume (`D3 -> D0`), GPU reset / TDR (`Win+Ctrl+Shift+B`), `taskkill /F` cleanup via `EvtFileCleanup`, Lock Screen (`Win+L`), and post-reboot recovery
- End-to-end LizardByte Sunshine + Moonlight streaming and clean MSI install/upgrade/uninstall

## Mock vs. Real Hardware Coverage

| Subsystem | Tier 1 (Mock / Pure Rust) | Tier 2 (WDK / Build) | Tier 3 (Live Windows HIL) |
|---|---|---|---|
| IOCTL ABI & IPC framing | Unit + proptest + fuzz corpus | Native Windows compile | Raw IOCTL fuzz + ACL denial |
| EDID 1.4 + CTA-861-G + HDR10 | Checksum, PPI, chromaticity, proptest | Included in driver DLL | Windows EDID parse, DPI, HDR |
| CVT-RBv2 64-bit timing math | 8K@240Hz, fractional Hz, even scaling | Included in driver DLL | `EnumDisplaySettingsExW` parity |
| Driver state machine | `FakeIddPlatform` re-entrancy, rollback, soak | Linked into `zerovdd.dll` | Live `WUDFHost.exe` soak + TDR |
| CCD & Win11 24H2 HDR/ACM | Captured topology fixtures | Native `windows` compile | Live multi-monitor & HDR toggle |
| Session-0 service & trampoline | State machine + process-tree mocks | Native Windows service build | `taskkill /F`, Lock Screen, reboot |
| INF & UMDF compliance | `crate_dag_lint`, `unsafe_safety_comment_lint` | `InfVerif /u /h`, `ApiValidator` | Secure Boot install/upgrade/uninstall |

## Test Authoring Rules

- **Zero `sleep` in tests:** Never call `std::thread::sleep` or async sleep in unit/integration tests. Use `std::sync::Barrier`, channels, or injected monotonic clocks (`test_sleep_lint`).
- **Distinguishing negative checks:** Every static lint and validation test must include at least one known-bad fixture proving that invalid inputs are rejected.
- **No external network or host state mutation in Tier 1:** Unit and script tests run in isolated `tempfile::TempDir` directories and clean up automatically.
