---
title: Signing and Release Runbook
description: Three-stage Windows code signing model (dev test-signing, SignPath Foundation, Microsoft Partner Center EV Attestation), WDK validation gates, and version lockstep.
---

Windows enforces separate trust models for user-mode executables/installers and Plug-and-Play (`Class=Display`) Universal Drivers. ZeroVDD uses a three-stage signing model so local builds stay fast while public releases satisfy retail Windows 11 security policies.

## Three-Stage Signing Model

### Stage A: Local Development & CI Test Signing

Used for local builds, Windows CI validation, and developer test machines.

- **Build script:** `scripts/windows/build-driver.ps1`
  - Builds `driver-wdk` (`zerovdd.dll`) for `x64` (`x86_64-pc-windows-msvc`) and `ARM64` (`aarch64-pc-windows-msvc`).
  - Runs `stampinf.exe`, `infverif.exe` (`/u`, `/h`, `/w`), and `apivalidator.exe`.
  - Signs `zerovdd.dll` via `scripts/windows/sign-dev.ps1` **before** `Inf2Cat.exe` (so `zerovdd.cat` hashes the signed DLL), runs `Inf2Cat.exe`, and signs `zerovdd.cat` **after**.
- **Pinned certificate import:** `zerovdd setup install` imports a local dev cert only when explicitly given `--dev-cert <PATH>` and its matching 64-character `--expected-sha256 <HEX>`, tracking it for removal on `zerovdd setup uninstall`.
- **HVCI / Secure Boot caveat:** Importing a self-signed cert satisfies PnP Driver Store staging, but Windows 11 Memory Integrity (HVCI) with `testsigning` off blocks self-signed DLLs inside `WUDFHost.exe` (Device Manager Code `31`). Live Stage A testing requires `bcdedit /set testsigning on` on a dev machine or VM.

### Stage B: Public Releases (`SignPath Foundation` / Trusted Authenticode CA)

Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).

Used for public releases of `zerovdd.exe`, `zerovdd-svc.exe`, `zerovdd-tray.exe`, the WiX v5 MSI installer, **and** `zerovdd.dll` + `zerovdd.cat`:

- Verified on Windows 11 24H2 (`build 26300`, Secure Boot ON, HVCI ON, `testsigning` OFF): signing `zerovdd.dll` (before `Inf2Cat.exe`) and `zerovdd.cat` (after `Inf2Cat.exe`) with a trusted OSS/commercial Authenticode CA + RFC 3161 timestamp passes `WUDFHost.exe` Code Integrity without `testsigning`.
- **Scope boundary:** Stage B does not grant Microsoft WHQL logo status or Windows Update publishing; Stage C covers those targets.
- See [SignPath Onboarding](/release/signpath-onboarding/) for full setup instructions.

### Stage C: Production GA (`Microsoft Partner Center` EV Attestation / HLK)

Used for Microsoft-signed catalog distribution (Windows Update, Windows Server, strict WDAC / anti-cheat environments):

- **EV Attestation Signing:** Submit an EV-signed `.cab` built by `scripts/windows/build-attestation-cab.ps1` containing clean unsigned `zerovdd.dll`, `zerovdd.inf`, and `zerovdd.pdb` for `x64` and `ARM64` (`build-driver.ps1 -Arch x64,ARM64 -Profile release -SkipSign`).
- **HLK / WHQL Certification:** Required for Windows Update publishing and "Windows Certified" logo status via `.hlkx` submission.

## Driver Validation Gates (`InfVerif` & `ApiValidator`)

Every Windows CI run and local `build-driver.ps1` execution enforces:

1. **`InfVerif` (separate single-flag passes for WDK 10.0.26100+):**
   - `infverif.exe /v /u zerovdd.inf` (Universal INF rules)
   - `infverif.exe /v /h zerovdd.inf` (Hardware / PnP rules)
   - `infverif.exe /v /w zerovdd.inf` (Driver Store `DIRID 13` isolation)
2. **`ApiValidator`:** Runs `apivalidator.exe` against `zerovdd.dll` on both `x64` and `ARM64`, requiring `ApiValidation: All binaries are Universal`.

## Version Lockstep & Release Pipeline

All releasable manifests must carry the exact same semantic version (enforced by `scripts/check-versions.sh`):

- Root `Cargo.toml`
- `driver-wdk/Cargo.toml`
- `driver-wdk/zerovdd.inf` (`DriverVer`)
- `installer/zerovdd.wxs`
- Git release tag (`vX.Y.Z`)

Verify locally before tagging:

```bash
bash scripts/check-versions.sh --tag v0.1.0
mise run release:preflight
```

Pushing a `v*.*.*` tag runs `.github/workflows/release.yml` across `verify-versions`, `build-windows` (`x64` & `ARM64` binaries, driver validation, three-stage SignPath signing, WiX MSI, portable ZIP, CycloneDX SBOMs), `build-attestation-cab`, and `draft-release` (`SHA256SUMS.txt` + draft GitHub Release).
