[CmdletBinding()]
param(
    [string[]]$Arch = @("x64", "ARM64"),

    [ValidateSet("debug", "release")]
    [string]$Profile = "release",

    [switch]$SkipSign,

    [string]$OutputDir = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$DriverWdkDir = Join-Path $RepoRoot "driver-wdk"
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $RepoRoot "dist"
}

function Normalize-ArchList {
    param([string[]]$InputArch)
    $list = New-Object System.Collections.Generic.List[string]
    foreach ($item in $InputArch) {
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
    return $list | Select-Object -Unique
}

function Resolve-WdkRoot {
    $candidates = New-Object System.Collections.Generic.List[string]
    if (-not [string]::IsNullOrWhiteSpace($env:WDKContentRoot)) {
        $candidates.Add($env:WDKContentRoot)
    }
    $candidates.Add("C:\Program Files (x86)\Windows Kits\10")
    $candidates.Add((Join-Path $env:LOCALAPPDATA "zerovdd-wdk\kit"))

    foreach ($root in $candidates) {
        if (-not (Test-Path $root)) {
            continue
        }
        $iddcxHeaders = Get-ChildItem (Join-Path $root "Include\10.0.*\um\iddcx\1.10\IddCx.h") -ErrorAction SilentlyContinue
        if ($iddcxHeaders) {
            return (Resolve-Path $root).Path
        }
    }
    throw "Could not locate Windows 11 WDK with IddCx 1.10 headers. Checked WDKContentRoot, Program Files, and LOCALAPPDATA\zerovdd-wdk\kit."
}

function Resolve-SdkVersion {
    param([string]$WdkRoot)
    $matches = Get-ChildItem (Join-Path $WdkRoot "Include\10.0.*\um\iddcx\1.10\IddCx.h") -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending
    $first = $matches | Select-Object -First 1
    if (-not $first) {
        throw "No SDK version containing um\iddcx\1.10\IddCx.h found under $WdkRoot\Include"
    }
    # WHY: Extract `10.0.xxxxx.0` directory name four levels above `IddCx.h`.
    return $first.Directory.Parent.Parent.Parent.Name
}

function Resolve-ToolPath {
    param(
        [string]$WdkRoot,
        [string]$SdkVersion,
        [string[]]$RelativePatterns,
        [string]$FallbackCommand
    )
    foreach ($pattern in $RelativePatterns) {
        $expanded = $pattern.Replace("{SDK}", $SdkVersion)
        $matches = Get-ChildItem (Join-Path $WdkRoot $expanded) -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending
        $first = $matches | Select-Object -First 1
        if ($first) {
            return $first.FullName
        }
    }
    if (-not [string]::IsNullOrWhiteSpace($FallbackCommand)) {
        $cmd = Get-Command $FallbackCommand -ErrorAction SilentlyContinue
        if ($cmd) {
            return $cmd.Source
        }
    }
    throw "Could not locate tool matching patterns: $($RelativePatterns -join ', ')"
}

function Resolve-LlvmBin {
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA "zerovdd-wdk\llvm\SourceDir\LLVM\bin"),
        "C:\Program Files\LLVM\bin"
    )
    foreach ($dir in $candidates) {
        if ((Test-Path (Join-Path $dir "clang-cl.exe")) -and (Test-Path (Join-Path $dir "lld-link.exe"))) {
            return $dir
        }
    }
    $clang = Get-Command "clang-cl.exe" -ErrorAction SilentlyContinue
    if ($clang) {
        return (Split-Path $clang.Source -Parent)
    }
    return $null
}

function Get-DriverCargoVersion {
    param([string]$CargoTomlPath)
    $inPkg = $false
    foreach ($line in (Get-Content $CargoTomlPath)) {
        if ($line -match '^\[workspace\.package\]') {
            $inPkg = $true
            continue
        }
        if ($line -match '^\[') {
            $inPkg = $false
            continue
        }
        if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }
    throw "Could not extract [workspace.package] version from $CargoTomlPath"
}

$ArchList = Normalize-ArchList -InputArch $Arch
$WdkRoot = Resolve-WdkRoot
$SdkVersion = Resolve-SdkVersion -WdkRoot $WdkRoot
$StampInf = Resolve-ToolPath -WdkRoot $WdkRoot -SdkVersion $SdkVersion -RelativePatterns @("bin\{SDK}\x64\stampinf.exe", "bin\*\x64\stampinf.exe") -FallbackCommand "stampinf.exe"
$InfVerif = Resolve-ToolPath -WdkRoot $WdkRoot -SdkVersion $SdkVersion -RelativePatterns @("Tools\{SDK}\x64\infverif.exe", "Tools\*\x64\infverif.exe") -FallbackCommand "infverif.exe"
$ApiValidator = Resolve-ToolPath -WdkRoot $WdkRoot -SdkVersion $SdkVersion -RelativePatterns @("bin\{SDK}\x64\apivalidator.exe", "bin\*\x64\apivalidator.exe") -FallbackCommand "apivalidator.exe"
$Inf2Cat = Resolve-ToolPath -WdkRoot $WdkRoot -SdkVersion $SdkVersion -RelativePatterns @("bin\{SDK}\x86\Inf2Cat.exe", "bin\*\x86\Inf2Cat.exe") -FallbackCommand "Inf2Cat.exe"
$SignTool = Resolve-ToolPath -WdkRoot $WdkRoot -SdkVersion $SdkVersion -RelativePatterns @("bin\{SDK}\x64\signtool.exe", "bin\*\x64\signtool.exe") -FallbackCommand "signtool.exe"

$LocalCrtRoot = Join-Path $env:LOCALAPPDATA "zerovdd-wdk\crt"
$HasLocalCrt = Test-Path (Join-Path $LocalCrtRoot "crt\include")
$LlvmBin = Resolve-LlvmBin

Write-Host "[build-driver] WDK root    : $WdkRoot (SDK $SdkVersion)"
Write-Host "[build-driver] Local CRT   : $(if ($HasLocalCrt) { $LocalCrtRoot } else { 'using host/VS environment' })"
Write-Host "[build-driver] LLVM bin    : $(if ($LlvmBin) { $LlvmBin } else { 'using host PATH' })"
Write-Host "[build-driver] Profile     : $Profile"
Write-Host "[build-driver] Archs       : $($ArchList -join ', ')"

$env:WDKContentRoot = $WdkRoot
if ($LlvmBin) {
    $env:LIBCLANG_PATH = $LlvmBin
    $env:PATH = "$LlvmBin;$env:PATH"
    if (-not (Get-Command "cl.exe" -ErrorAction SilentlyContinue)) {
        $env:CC = "clang-cl"
    }
    if (-not (Get-Command "link.exe" -ErrorAction SilentlyContinue)) {
        $lldLink = Join-Path $LlvmBin "lld-link.exe"
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $lldLink
        $env:CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER = $lldLink
    }
}

$BaseVersion = Get-DriverCargoVersion -CargoTomlPath (Join-Path $DriverWdkDir "Cargo.toml")
$CoreParts = ($BaseVersion -split "-")[0]
$FourPartVersion = "$CoreParts.0"

$OrigInclude = $env:INCLUDE
$OrigLib = $env:LIB

foreach ($archName in $ArchList) {
    $env:INCLUDE = $OrigInclude
    $env:LIB = $OrigLib

    if ($archName -eq "x64") {
        $rustTarget = "x86_64-pc-windows-msvc"
        $wdkArch = "x64"
        $stampArch = "amd64"
        $inf2CatOs = "10_NI_X64,10_GE_X64"
    }
    else {
        $rustTarget = "aarch64-pc-windows-msvc"
        $wdkArch = "arm64"
        $stampArch = "arm64"
        $inf2CatOs = "10_NI_ARM64,10_GE_ARM64"
    }

    Write-Host "`n=== Building $archName ($rustTarget) ==="

    $includeList = New-Object System.Collections.Generic.List[string]
    $includeList.Add((Join-Path $WdkRoot "Include\$SdkVersion\um"))
    $includeList.Add((Join-Path $WdkRoot "Include\$SdkVersion\shared"))
    $includeList.Add((Join-Path $WdkRoot "Include\wdf\umdf\2.33"))
    if ($HasLocalCrt) {
        $includeList.Add((Join-Path $LocalCrtRoot "crt\include"))
        $includeList.Add((Join-Path $LocalCrtRoot "sdk\include\ucrt"))
        $includeList.Add((Join-Path $LocalCrtRoot "sdk\include\um"))
        $includeList.Add((Join-Path $LocalCrtRoot "sdk\include\shared"))
    }
    elseif (-not [string]::IsNullOrWhiteSpace($env:INCLUDE)) {
        foreach ($p in ($env:INCLUDE -split ";")) {
            if (-not [string]::IsNullOrWhiteSpace($p)) {
                $includeList.Add($p)
            }
        }
    }

    $libList = New-Object System.Collections.Generic.List[string]
    $libList.Add((Join-Path $WdkRoot "Lib\$SdkVersion\um\$wdkArch"))
    $libList.Add((Join-Path $WdkRoot "Lib\wdf\umdf\$wdkArch\2.33"))
    if ($HasLocalCrt) {
        $libList.Add((Join-Path $LocalCrtRoot "crt\lib\$wdkArch"))
        $libList.Add((Join-Path $LocalCrtRoot "sdk\lib\ucrt\$wdkArch"))
        $libList.Add((Join-Path $LocalCrtRoot "sdk\lib\um\$wdkArch"))
    }
    elseif (-not [string]::IsNullOrWhiteSpace($env:LIB)) {
        foreach ($p in ($env:LIB -split ";")) {
            if (-not [string]::IsNullOrWhiteSpace($p)) {
                $libList.Add($p)
            }
        }
    }

    $env:INCLUDE = ($includeList | Select-Object -Unique) -join ";"
    $env:LIB = ($libList | Select-Object -Unique) -join ";"

    $cargoArgs = @("build", "--target", $rustTarget)
    if ($Profile -eq "release") {
        $cargoArgs += "--release"
    }

    Push-Location $DriverWdkDir
    try {
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed for $rustTarget with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }

    $targetOutDir = Join-Path $DriverWdkDir "target\$rustTarget\$Profile"
    $dllSrc = Join-Path $targetOutDir "zerovdd.dll"
    $pdbSrc = Join-Path $targetOutDir "zerovdd.pdb"
    if (-not (Test-Path $dllSrc)) {
        throw "Expected driver DLL not found at $dllSrc"
    }

    $pkgDir = Join-Path $OutputDir $archName
    New-Item -ItemType Directory -Force -Path $pkgDir | Out-Null
    Copy-Item $dllSrc (Join-Path $pkgDir "zerovdd.dll") -Force
    if (Test-Path $pdbSrc) {
        Copy-Item $pdbSrc (Join-Path $pkgDir "zerovdd.pdb") -Force
    }
    $stagedInf = Join-Path $pkgDir "zerovdd.inf"
    Copy-Item (Join-Path $DriverWdkDir "zerovdd.inf") $stagedInf -Force

    Write-Host "[build-driver] Running StampInf ($stampArch, v$FourPartVersion)..."
    & $StampInf -f $stagedInf -a $stampArch -c "zerovdd.cat" -d "11/24/2026" -v $FourPartVersion
    if ($LASTEXITCODE -ne 0) {
        throw "stampinf.exe failed with exit code $LASTEXITCODE"
    }

    # WHY: WDK 10.0.26100 InfVerif accepts only one mode flag per invocation, so /u, /h, and /w
    # are executed sequentially to enforce Universal, WHQL, and DIRID 13 Driver Store Isolation.
    foreach ($modeFlag in @("/u", "/h", "/w")) {
        Write-Host "[build-driver] Running InfVerif $modeFlag on $stagedInf..."
        $ivOut = & $InfVerif /v $modeFlag $stagedInf 2>&1
        $ivRc = $LASTEXITCODE
        $ivOut | ForEach-Object { Write-Host $_ }
        if ($ivRc -ne 0 -or -not (($ivOut -join "`n") -match "INF is VALID")) {
            throw "InfVerif $modeFlag failed for $stagedInf (exit code $ivRc)"
        }
    }

    $universalXml = Join-Path $WdkRoot "build\$SdkVersion\universalDDIs\$wdkArch\UniversalDDIs.xml"
    $whitelistXml = Join-Path $WdkRoot "build\$SdkVersion\universalDDIs\$wdkArch\ModuleWhitelist.xml"
    if (-not (Test-Path $universalXml)) {
        throw "UniversalDDIs.xml not found at $universalXml"
    }

    $stagedDll = Join-Path $pkgDir "zerovdd.dll"
    $apiArgs = @(
        "-BinaryPath:$stagedDll",
        "-SupportedApiXmlFiles:$universalXml"
    )
    if (Test-Path $whitelistXml) {
        $apiArgs += "-ModuleWhiteListXmlFiles:$whitelistXml"
    }
    Write-Host "[build-driver] Running ApiValidator ($wdkArch)..."
    & $ApiValidator @apiArgs
    if ($LASTEXITCODE -ne 0) {
        throw "ApiValidator failed for $archName with exit code $LASTEXITCODE"
    }

    if (-not $SkipSign) {
        & (Join-Path $PSScriptRoot "sign-dev.ps1") -PackageDir $pkgDir -TargetFiles @($stagedDll) -SignToolPath $SignTool
        if ($LASTEXITCODE -ne 0) {
            throw "sign-dev.ps1 failed for $stagedDll with exit code $LASTEXITCODE"
        }
    }

    Write-Host "[build-driver] Running Inf2Cat (/os:$inf2CatOs)..."
    & $Inf2Cat /driver:$pkgDir /os:$inf2CatOs
    if ($LASTEXITCODE -ne 0) {
        throw "Inf2Cat failed for $archName with exit code $LASTEXITCODE"
    }

    if (-not $SkipSign) {
        $stagedCat = Join-Path $pkgDir "zerovdd.cat"
        & (Join-Path $PSScriptRoot "sign-dev.ps1") -PackageDir $pkgDir -TargetFiles @($stagedCat) -SignToolPath $SignTool
        if ($LASTEXITCODE -ne 0) {
            throw "sign-dev.ps1 failed for $stagedCat with exit code $LASTEXITCODE"
        }
    }
}

Write-Host "`n[build-driver] All requested architectures ($($ArchList -join ', ')) succeeded."
