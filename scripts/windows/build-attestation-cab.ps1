[CmdletBinding()]
param(
    [string]$DistDir = "dist",

    [string]$OutDir = "dist",

    [string]$Version = "0.1.0",

    [string[]]$Archs = @("x64", "ARM64"),

    [string]$PfxPath = "",

    [string]$PfxPassword = "",

    [string]$CertThumbprint = "",

    [string]$TimestampUrl = "http://timestamp.digicert.com"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-FullPath {
    param([string]$InputPath)
    if ([System.IO.Path]::IsPathRooted($InputPath)) {
        return [System.IO.Path]::GetFullPath($InputPath)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $InputPath))
}

function Normalize-ArchList {
    param([string[]]$InputArchs)
    $list = New-Object System.Collections.Generic.List[string]
    foreach ($item in $InputArchs) {
        foreach ($part in ($item -split ",")) {
            $trimmed = $part.Trim()
            if ([string]::IsNullOrWhiteSpace($trimmed)) {
                continue
            }
            switch -Exact ($trimmed.ToLowerInvariant()) {
                "x64" { $list.Add("x64") }
                "amd64" { $list.Add("x64") }
                "x86_64" { $list.Add("x64") }
                "arm64" { $list.Add("ARM64") }
                "aarch64" { $list.Add("ARM64") }
                default { throw "Unsupported architecture: '$trimmed' (expected x64 or ARM64)" }
            }
        }
    }
    if ($list.Count -eq 0) {
        throw "No architectures specified."
    }
    return @($list | Select-Object -Unique)
}

function Resolve-SignTool {
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

if ([string]::IsNullOrWhiteSpace($Version)) {
    throw "Version must not be empty."
}
if (-not [string]::IsNullOrWhiteSpace($PfxPath) -and -not [string]::IsNullOrWhiteSpace($CertThumbprint)) {
    throw "Specify either -PfxPath or -CertThumbprint, not both."
}

$ArchList = Normalize-ArchList -InputArchs $Archs
$ResolvedDistDir = Resolve-FullPath -InputPath $DistDir
$ResolvedOutDir = Resolve-FullPath -InputPath $OutDir

if (-not (Test-Path $ResolvedDistDir)) {
    throw "Distribution directory not found: $ResolvedDistDir"
}

New-Item -ItemType Directory -Force -Path $ResolvedOutDir | Out-Null

$CabFileName = "zerovdd-attestation-$Version.cab"
$CabPath = Join-Path $ResolvedOutDir $CabFileName
$RequiredFiles = @("zerovdd.dll", "zerovdd.inf", "zerovdd.pdb")

$WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) ("zerovdd-cab-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

try {
    $DdfPath = Join-Path $WorkDir "attestation.ddf"
    $InfReportPath = Join-Path $WorkDir "setup.inf"
    $RptPath = Join-Path $WorkDir "setup.rpt"

    $ddfLines = New-Object System.Collections.Generic.List[string]
    $ddfLines.Add(".OPTION EXPLICIT")
    $ddfLines.Add(".Set CabinetNameTemplate=$CabFileName")
    $ddfLines.Add(".Set DiskDirectory1=`"$ResolvedOutDir`"")
    $ddfLines.Add(".Set CompressionType=MSZIP")
    $ddfLines.Add(".Set Cabinet=on")
    $ddfLines.Add(".Set Compress=on")
    $ddfLines.Add(".Set MaxDiskSize=0")
    $ddfLines.Add(".Set FolderSizeThreshold=0")
    $ddfLines.Add(".Set InfFileName=`"$InfReportPath`"")
    $ddfLines.Add(".Set RptFileName=`"$RptPath`"")

    $expectedEntries = New-Object System.Collections.Generic.List[string]

    foreach ($archName in $ArchList) {
        $archDir = Join-Path $ResolvedDistDir $archName
        if (-not (Test-Path $archDir)) {
            throw "Architecture directory not found: $archDir"
        }

        $ddfLines.Add(".Set DestinationDir=$archName")
        foreach ($fileName in $RequiredFiles) {
            $fullFile = Join-Path $archDir $fileName
            if (-not (Test-Path $fullFile)) {
                throw "Required attestation artifact missing: $fullFile"
            }
            $ddfLines.Add("`"$fullFile`"")
            $expectedEntries.Add("$archName\$fileName")
        }
    }

    # WHY: MakeCAB expects ANSI/ASCII or UTF-8 without BOM for directive files.
    [System.IO.File]::WriteAllLines($DdfPath, $ddfLines, (New-Object System.Text.UTF8Encoding($false)))

    Write-Host "[build-attestation-cab] Building $CabPath via makecab.exe..."
    $makecabCmd = Get-Command "makecab.exe" -ErrorAction Stop
    & $makecabCmd.Source /F $DdfPath
    if ($LASTEXITCODE -ne 0) {
        throw "makecab.exe failed with exit code $LASTEXITCODE"
    }

    if (-not (Test-Path $CabPath)) {
        throw "Expected attestation CAB was not created at $CabPath"
    }

    if (-not [string]::IsNullOrWhiteSpace($PfxPath) -or -not [string]::IsNullOrWhiteSpace($CertThumbprint)) {
        $signTool = Resolve-SignTool
        $signArgs = @("sign", "/fd", "SHA256", "/tr", $TimestampUrl, "/td", "SHA256")
        if (-not [string]::IsNullOrWhiteSpace($PfxPath)) {
            $resolvedPfx = Resolve-FullPath -InputPath $PfxPath
            if (-not (Test-Path $resolvedPfx)) {
                throw "PFX file not found: $resolvedPfx"
            }
            $signArgs += @("/f", $resolvedPfx)
            if (-not [string]::IsNullOrWhiteSpace($PfxPassword)) {
                $signArgs += @("/p", $PfxPassword)
            }
        }
        else {
            $signArgs += @("/sha1", $CertThumbprint)
        }
        $signArgs += $CabPath

        Write-Host "[build-attestation-cab] Signing $CabFileName with signtool.exe..."
        & $signTool @signArgs
        if ($LASTEXITCODE -ne 0) {
            throw "signtool.exe failed with exit code $LASTEXITCODE"
        }
    }
    else {
        Write-Host "[build-attestation-cab] No signing credentials supplied; leaving $CabFileName unsigned."
    }

    Write-Host "[build-attestation-cab] Verifying cabinet contents via expand.exe -D..."
    $expandCmd = Get-Command "expand.exe" -ErrorAction Stop
    $expandOutput = & $expandCmd.Source -D $CabPath 2>&1
    $expandRc = $LASTEXITCODE
    $expandOutput | ForEach-Object { Write-Host $_ }
    if ($expandRc -ne 0) {
        throw "expand.exe -D failed with exit code $expandRc"
    }

    # WHY: `expand.exe -D` prints basenames only and uses localized summary lines, so expand into a scratch
    # directory to verify both total file count and exact per-architecture paths in a locale-independent way.
    $verifyDir = Join-Path $WorkDir "verify"
    New-Item -ItemType Directory -Force -Path $verifyDir | Out-Null
    & $expandCmd.Source $CabPath -F:* $verifyDir | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "expand.exe -F:* verification extract failed with exit code $LASTEXITCODE"
    }
    $extractedFiles = @(Get-ChildItem -Path $verifyDir -Recurse -File)
    if ($extractedFiles.Count -ne $expectedEntries.Count) {
        throw "Cabinet verification failed: expected $($expectedEntries.Count) files, found $($extractedFiles.Count) in $CabPath"
    }
    foreach ($entry in $expectedEntries) {
        $extractedFile = Join-Path $verifyDir $entry
        if (-not (Test-Path $extractedFile)) {
            throw "Cabinet verification failed: missing expected entry '$entry' in $CabPath"
        }
    }

    Write-Host "[build-attestation-cab] Verified $($expectedEntries.Count) entries in $CabPath."
}
finally {
    if (Test-Path $WorkDir) {
        Remove-Item -Recurse -Force $WorkDir -ErrorAction SilentlyContinue
    }
}
