# WinGet Packaging and Submission Guide (`zerovdd`)

This directory documents the Windows Package Manager (`winget-pkgs`) manifest templates (stored under `packaging/winget/templates/`) and release submission workflow for `zerovdd`.

## Package Identity

- **PackageIdentifier:** `ZeroVDD.ZeroVDD`
- **UpgradeCode:** `{E3B14E29-B75E-4BCC-8194-F71147EFD27B}` (defined in `installer/zerovdd.wxs`)
- **InstallerType:** `msi`
- **Scope:** `machine` (per-machine installation under `ProgramFiles6432Folder`)

## Manifest Templates (WinGet Schema 1.9.0)

WinGet uses a three-file multi-file manifest layout under `manifests/z/ZeroVDD/ZeroVDD/<PackageVersion>/` in `microsoft/winget-pkgs`:

### 1. `ZeroVDD.ZeroVDD.yaml` (Version Manifest)

```yaml
# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.1.9.0.schema.json
PackageIdentifier: ZeroVDD.ZeroVDD
PackageVersion: 0.1.0
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.9.0
```

### 2. `ZeroVDD.ZeroVDD.locale.en-US.yaml` (Default Locale Manifest)

```yaml
# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.1.9.0.schema.json
PackageIdentifier: ZeroVDD.ZeroVDD
PackageVersion: 0.1.0
PackageLocale: en-US
Publisher: ZeroVDD Contributors
PublisherUrl: https://zerovdd.dev
PublisherSupportUrl: https://github.com/zerovdd/zerovdd/issues
PackageName: ZeroVDD Virtual Display Driver
PackageUrl: https://zerovdd.dev
License: MIT OR Apache-2.0
LicenseUrl: https://github.com/zerovdd/zerovdd/blob/main/LICENSE
ShortDescription: Virtual Display Driver (IddCx 1.10 / UMDF 2) with HDR10, custom EDIDs, and Sunshine integration.
Tags:
  - display
  - virtual-display
  - iddcx
  - hdr
  - sunshine
ManifestType: defaultLocale
ManifestVersion: 1.9.0
```

### 3. `ZeroVDD.ZeroVDD.installer.yaml` (Installer Manifest)

```yaml
# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.1.9.0.schema.json
PackageIdentifier: ZeroVDD.ZeroVDD
PackageVersion: 0.1.0
InstallerType: msi
Scope: machine
UpgradeBehavior: install
ElevationRequirement: elevatesSelf
ProductCode: "{REPLACE-WITH-MSI-PRODUCT-CODE}"
AppsAndFeaturesEntries:
  - DisplayName: ZeroVDD Virtual Display Driver
    Publisher: ZeroVDD Contributors
    ProductCode: "{REPLACE-WITH-MSI-PRODUCT-CODE}"
    UpgradeCode: "{E3B14E29-B75E-4BCC-8194-F71147EFD27B}"
Installers:
  - Architecture: x64
    InstallerUrl: https://github.com/zerovdd/zerovdd/releases/download/v0.1.0/zerovdd-0.1.0-x64.msi
    InstallerSha256: REPLACE_WITH_X64_MSI_SHA256
    ProductCode: "{REPLACE-WITH-X64-MSI-PRODUCT-CODE}"
  - Architecture: arm64
    InstallerUrl: https://github.com/zerovdd/zerovdd/releases/download/v0.1.0/zerovdd-0.1.0-ARM64.msi
    InstallerSha256: REPLACE_WITH_ARM64_MSI_SHA256
    ProductCode: "{REPLACE-WITH-ARM64-MSI-PRODUCT-CODE}"
ManifestType: installer
ManifestVersion: 1.9.0
```

## Release Submission Steps (`wingetcreate`)

1. **Publish the GitHub Release:**
   Verify the draft release created by `.github/workflows/release.yml`, confirm `SHA256SUMS.txt`, `.msi` installers, and `.cab` signing status, and publish the release on GitHub.

2. **Collect SHA-256 Hashes and `ProductCode` Values:**
   Download `zerovdd-<version>-x64.msi`, `zerovdd-<version>-ARM64.msi`, and `SHA256SUMS.txt` from the published release:

   ```powershell
   Get-FileHash .\zerovdd-0.1.0-x64.msi -Algorithm SHA256
   Get-FileHash .\zerovdd-0.1.0-ARM64.msi -Algorithm SHA256

   # Inspect MSI ProductCode using WindowsInstaller COM
   $wi = New-Object -ComObject WindowsInstaller.Installer
   $db = $wi.OpenDatabase((Resolve-Path .\zerovdd-0.1.0-x64.msi).Path, 0)
   $view = $db.OpenView("SELECT Value FROM Property WHERE Property='ProductCode'")
   $view.Execute()
   $view.Fetch().StringData(1)
   ```

3. **Install or Update `wingetcreate`:**

   ```powershell
   winget install Microsoft.WingetCreate
   ```

4. **Generate or Update the Manifest:**
   - For the initial submission:

     ```powershell
     wingetcreate new `
       https://github.com/zerovdd/zerovdd/releases/download/v0.1.0/zerovdd-0.1.0-x64.msi `
       https://github.com/zerovdd/zerovdd/releases/download/v0.1.0/zerovdd-0.1.0-ARM64.msi
     ```

   - For subsequent releases:

     ```powershell
     wingetcreate update ZeroVDD.ZeroVDD `
       --version 0.1.0 `
       --urls `
         https://github.com/zerovdd/zerovdd/releases/download/v0.1.0/zerovdd-0.1.0-x64.msi `
         https://github.com/zerovdd/zerovdd/releases/download/v0.1.0/zerovdd-0.1.0-ARM64.msi
     ```

5. **Validate Locally Before Submitting:**

   ```powershell
   winget validate --manifest .\manifests\z\ZeroVDD\ZeroVDD\0.1.0\
   ```

   Optionally test local installation on a clean Windows VM with WinGet local manifest support enabled:

   ```powershell
   winget settings --enable LocalManifestFiles
   winget install --manifest .\manifests\z\ZeroVDD\ZeroVDD\0.1.0\
   ```

6. **Submit Pull Request to `microsoft/winget-pkgs`:**
   Use `wingetcreate submit` (or pass `--submit` directly to `wingetcreate update`) using a GitHub Personal Access Token with `public_repo` scope:

   ```powershell
   wingetcreate submit .\manifests\z\ZeroVDD\ZeroVDD\0.1.0\
   ```
