---
title: Code Signing Policy
description: Governance, signed artifact scope, maintainer roles, build provenance, and offline privacy policy for official ZeroVDD releases signed via SignPath Foundation.
---

Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).

## Scope & Signed Artifacts

Signing keys issued via SignPath Foundation are used strictly for official `ZeroVDD` (`zerovdd`) builds produced in CI from tagged releases of `https://github.com/zerovdd/zerovdd`:

- User-mode binaries: `zerovdd.exe`, `zerovdd-svc.exe`, `zerovdd-tray.exe`
- UMDF 2 / IddCx 1.10 driver module: `zerovdd.dll`
- Plug-and-Play catalog generated after DLL signing: `zerovdd.cat`
- Windows Installer package: `zerovdd-<version>-<arch>.msi`

No third-party binaries or upstream dependencies are re-signed with SignPath certificates. Clean unsigned driver inputs (`driver-raw-<arch>`) are retained separately before signing for Microsoft Partner Center attestation CAB packaging.

## Team Roles & Access Controls

| Role | Scope & Responsibility | Enforcement |
| :--- | :--- | :--- |
| **Committers / Authors** | Repository maintainers (`zerovdd` GitHub write access) who author commits and pull requests. | Protected `main` branch; mandatory pre-commit lint and format gates. |
| **Reviewers** | Maintainers verifying code changes, WDK compliance, and CI checks prior to merge or release tagging. | All pull requests and release tags require clean CI verification before signing. |
| **Approvers** | Designated SignPath Foundation organization approvers authorized to release production signatures. | SignPath signing requests on `release-signing` require explicit human approval in SignPath with MFA enabled. |

Multi-factor authentication (MFA) is enforced for all accounts holding Commit, Review, or SignPath Approver privileges.

## Build Provenance & Verification

- **Source of truth:** Public GitHub repository `https://github.com/zerovdd/zerovdd` (`main` and annotated/pushed `v*.*.*` release tags).
- **Build environment:** GitHub-hosted Actions runners only (`ubuntu-latest` for version lockstep checks and checksums; `windows-latest` for binary, driver, catalog, and MSI builds). Self-hosted runners are not permitted for SignPath signing requests.
- **Pipeline immutability:** `.github/workflows/release.yml` pins all third-party GitHub Actions to 40-character commit SHAs and runs `scripts/check-versions.sh` before building artifacts.
- **Draft release review:** Signed installers, driver archives, CycloneDX SBOMs, and `SHA256SUMS.txt` are published to a draft GitHub Release for final maintainer verification before public release.

## Privacy Policy

`ZeroVDD` (`zerovdd`) operates strictly offline on the local Windows host:

- Zero telemetry, analytics, crash-reporting uploads, or background phone-home network traffic.
- No collection, transmission, or storage of personal data.
- Local state consists solely of user configuration (`%ProgramData%\zerovdd\config.toml`), optional pre-stream display topology backup snapshots (`topology-backup.json`), installer state (`install-state.json`), and standard Windows Event Tracing (ETW) diagnostic events on the local machine.
