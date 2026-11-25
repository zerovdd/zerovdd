# Crate Architecture and Rules (`crates/`)

This document specifies the crate dependency graph, module conventions, and safety boundaries enforced automatically by `crates/zerovdd-checks`.

## Acyclic Crate Dependency DAG

Dependencies flow strictly downward from higher layers to lower layers. Cycles and cross-layer violations fail `crate_dag_lint`.

- **Layer 0 (Pure domain & config, zero `windows` / `wdk` dependencies):**
  - `zerovdd-proto` (`#![no_std]` + `alloc`; `std` feature for IPC): Versioned binary IOCTL ABI and service IPC framing.
  - `zerovdd-edid` (`#![no_std]` + `alloc`): EDID 1.4 + CTA-861-G synthesizer, HDR10 metadata, and custom EDID validator.
  - `zerovdd-modes` (`#![no_std]` + `alloc`): 64-bit VESA CVT-RBv2 timing math, rational refresh rates, and unified mode planner.
  - `zerovdd-config` (`std`): Typed TOML configuration schema and atomic file persistence.
- **Layer 1 (Driver state engine, zero `windows` / `wdk` dependencies):**
  - `zerovdd-engine` (`#![no_std]` + `alloc`): Re-entrancy-safe monitor registry, lifecycle/D0-D3 state machine, lease/handle ownership, GPU-switch circuit breaker, and deterministic `FakeIddPlatform` simulator.
  - Allowed workspace deps: `zerovdd-proto`, `zerovdd-edid`, `zerovdd-modes`.
- **Layer 2 (Platform adapters):**
  - `driver-wdk/zerovdd-iddcx-sys` (WDK): UMDF 2 / IddCx 1.10 bindings and C inline-table trampoline.
  - `driver-wdk/zerovdd-driver` (WDK): UMDF 2 DLL, WDF Control Device, D3D11 device manager, swapchain worker, hardware cursor handler, and ETW TraceLogging. Allowed workspace deps: `zerovdd-engine`, `zerovdd-proto`, `zerovdd-edid`, `zerovdd-modes`, `zerovdd-iddcx-sys`. Must NEVER depend on `zerovdd-client`, `zerovdd-installer`, `zerovdd-cli`, or `zerovdd-svc`.
  - `zerovdd-client` (`cfg(windows)`): Control Device IOCTL client, CCD `QueryDisplayConfig`/`SetDisplayConfig`, Win11 24H2 HDR/ACM, and DXGI GPU selection. Allowed workspace deps: `zerovdd-proto`, `zerovdd-config`.
  - `zerovdd-installer` (`cfg(windows)`): Native SetupAPI/`newdev` driver package, device-node manager, and opt-in pinned dev-cert helper.
- **Layer 3 (Binaries):**
  - `zerovdd-cli` (`zerovdd.exe`): `setup`, `display`, `edid`, `sunshine`, `doctor`, and hidden `internal-ccd` subcommands. Allowed workspace deps: `zerovdd-client`, `zerovdd-installer`, `zerovdd-config`, `zerovdd-edid`, `zerovdd-proto`.
  - `zerovdd-svc` (`zerovdd-svc.exe`): Session-0 Windows service, ACL'd named pipe, active console-session CCD launcher, and `sunshine.exe` crash guardian. Allowed workspace deps: `zerovdd-client`, `zerovdd-config`, `zerovdd-edid`, `zerovdd-proto`.
  - `zerovdd-tray` (`zerovdd-tray.exe`): Native Win32 system tray companion communicating exclusively over `\\.\pipe\zerovdd-svc` (and launching elevated `zerovdd.exe` only for `Configure Sunshine...`). Allowed workspace deps: `zerovdd-client`, `zerovdd-config`, `zerovdd-proto`.
- **Verification Crate (Zero workspace dependencies):**
  - `zerovdd-checks`: Compiled static repository lints and hook/script integration tests. Must have zero internal workspace dependencies.

## Mandatory Crate Checklist

1. Declare `[lints] workspace = true` in every `Cargo.toml` (both `crates/*` and `driver-wdk/*`).
2. Use Rust 2018+ module layout (`foo.rs` + `foo/`); never introduce `mod.rs`.
3. Set `[lib] doctest = false` unless the crate explicitly maintains doctests.
4. Keep Layer 0/1 `#![no_std]` crates (`proto`, `edid`, `modes`, `engine`) compiling cleanly under `--no-default-features`.
5. Place Windows-only crate dependencies under `[target.'cfg(windows)'.dependencies]` in user-mode crates so Linux CI can run `cargo check --target x86_64-pc-windows-msvc` and portable unit tests cleanly.
6. Use `thiserror` for library errors and `anyhow` for binary entrypoints.
7. Document every public item (`///` / `//!`), explain non-obvious invariants with `// WHY:`, and justify every `unsafe` block or `unsafe fn` with `// SAFETY:`.
8. Follow the driver panic policy enforced by `driver_panic_policy_lint` inside `driver-wdk/zerovdd-driver`.
