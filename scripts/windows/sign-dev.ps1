[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageDir,

    [string[]]$TargetFiles = @(),

    [string]$CertDir = "",

    [string]$SignToolPath = "",

    [switch]$InstallToCurrentUserStore
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if ([string]::IsNullOrWhiteSpace($CertDir)) {
    $CertDir = Join-Path $RepoRoot "dist\dev-cert"
}

if (-not (Test-Path $PackageDir)) {
    throw "Package directory not found: $PackageDir"
}

function Resolve-SignTool {
    param([string]$ExplicitPath)
    if (-not [string]::IsNullOrWhiteSpace($ExplicitPath) -and (Test-Path $ExplicitPath)) {
        return $ExplicitPath
    }

    $candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($env:WDKContentRoot)) {
        $candidates += Get-ChildItem (Join-Path $env:WDKContentRoot "bin\*\x64\signtool.exe") -ErrorAction SilentlyContinue
    }
    $localKit = Join-Path $env:LOCALAPPDATA "zerovdd-wdk\kit"
    if (Test-Path $localKit) {
        $candidates += Get-ChildItem (Join-Path $localKit "bin\*\x64\signtool.exe") -ErrorAction SilentlyContinue
    }
    $pfKit = "C:\Program Files (x86)\Windows Kits\10"
    if (Test-Path $pfKit) {
        $candidates += Get-ChildItem (Join-Path $pfKit "bin\*\x64\signtool.exe") -ErrorAction SilentlyContinue
    }
    $found = $candidates | Sort-Object FullName -Descending | Select-Object -First 1
    if ($found) {
        return $found.FullName
    }
    $cmd = Get-Command "signtool.exe" -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }
    throw "Could not locate signtool.exe in WDK/SDK paths or PATH."
}

$SignTool = Resolve-SignTool -ExplicitPath $SignToolPath
New-Item -ItemType Directory -Force -Path $CertDir | Out-Null

$PfxPath = Join-Path $CertDir "zerovdd-dev.pfx"
$CerPath = Join-Path $CertDir "zerovdd-dev.cer"
$PfxPasswordText = "zerovdd-dev-signing"
$PfxPassword = ConvertTo-SecureString -String $PfxPasswordText -Force -AsPlainText

if (-not (Test-Path $PfxPath) -or -not (Test-Path $CerPath)) {
    Write-Host "[sign-dev] Generating user-scoped development certificate in $CertDir"
    $cert = New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject "CN=ZeroVDD Development Test Signing" `
        -KeyExportPolicy Exportable `
        -KeySpec Signature `
        -KeyLength 2048 `
        -HashAlgorithm SHA256 `
        -NotAfter (Get-Date).AddYears(3) `
        -CertStoreLocation "Cert:\CurrentUser\My"

    try {
        Export-PfxCertificate -Cert $cert -FilePath $PfxPath -Password $PfxPassword | Out-Null
        Export-Certificate -Cert $cert -FilePath $CerPath | Out-Null
    }
    finally {
        Remove-Item -Path ("Cert:\CurrentUser\My\" + $cert.Thumbprint) -Force -ErrorAction SilentlyContinue
    }
}

if ($InstallToCurrentUserStore) {
    Write-Host "[sign-dev] Importing $CerPath into CurrentUser\Root and CurrentUser\TrustedPublisher"
    Import-Certificate -FilePath $CerPath -CertStoreLocation "Cert:\CurrentUser\Root" | Out-Null
    Import-Certificate -FilePath $CerPath -CertStoreLocation "Cert:\CurrentUser\TrustedPublisher" | Out-Null
}

$targets = @()
if ($TargetFiles.Count -gt 0) {
    foreach ($filePath in $TargetFiles) {
        if (-not (Test-Path $filePath)) {
            throw "Target artifact not found: $filePath"
        }
        $targets += Get-Item -Path $filePath
    }
}
else {
    $targets += Get-ChildItem -Path $PackageDir -Filter "*.dll" -File
    $targets += Get-ChildItem -Path $PackageDir -Filter "*.cat" -File
}

if ($targets.Count -eq 0) {
    throw "No .dll or .cat artifacts found to sign in $PackageDir"
}

foreach ($artifact in $targets) {
    Write-Host "[sign-dev] Signing $($artifact.Name) with SHA-256..."
    & $SignTool sign /fd SHA256 /f $PfxPath /p $PfxPasswordText /v $artifact.FullName
    if ($LASTEXITCODE -ne 0) {
        throw "signtool failed for $($artifact.FullName) with exit code $LASTEXITCODE"
    }
}

Write-Host "[sign-dev] Signed $($targets.Count) artifact(s) in $PackageDir"
