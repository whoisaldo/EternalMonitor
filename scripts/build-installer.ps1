# EternalMonitor installer build script
# Produces a single EternalMonitor-Setup.exe that installs the host app, the FFmpeg
# runtime, and (if the driver is present) the bundled virtual display driver.
#
# Usage:  .\scripts\build-installer.ps1
#
# To bundle the virtual display driver, drop the signed setup into
#   installer\vendor\vdd\   (see that folder's README.txt for the exact file).
# Without it, the build still succeeds and produces an app-only installer.

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$version = "0.1.1"
$staging = Join-Path $repo "build\installer-staging"
$outDir  = Join-Path $repo "build\out"

# --- Locate the Inno Setup compiler ---------------------------------------------
$iscc = Get-Command iscc.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
if (-not $iscc) {
    foreach ($c in @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
        "C:\Program Files\Inno Setup 6\ISCC.exe")) {
        if (Test-Path $c) { $iscc = $c; break }
    }
}
if (-not $iscc) {
    throw "Inno Setup compiler (ISCC.exe) not found. Install it with: winget install JRSoftware.InnoSetup"
}
Write-Host "[setup] ISCC: $iscc"

# --- Build the release binary ----------------------------------------------------
Write-Host "[1/5] Building release binary..."
cargo build --release -p eternal-host
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# --- Stage the payload -----------------------------------------------------------
Write-Host "[2/5] Staging payload -> $staging"
if (Test-Path $staging) { Remove-Item $staging -Recurse -Force }
New-Item -ItemType Directory -Path $staging | Out-Null
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

Copy-Item "target\release\eternal-host.exe" (Join-Path $staging "EternalMonitor-host.exe")

# FFmpeg runtime DLLs + utility, resolved from .cargo/config.toml (same as package.ps1).
$cargoConfig = Get-Content -Raw ".cargo\config.toml"
$ffmpegMatch = [regex]::Match($cargoConfig, 'FFMPEG_DIR\s*=\s*\{\s*value\s*=\s*"([^"]+)"')
if (-not $ffmpegMatch.Success) {
    $ffmpegMatch = [regex]::Match($cargoConfig, 'FFMPEG_DIR\s*=\s*"([^"]+)"')
}
if (-not $ffmpegMatch.Success) { throw "Could not resolve FFMPEG_DIR from .cargo/config.toml" }
$ffmpegBin = Join-Path $ffmpegMatch.Groups[1].Value "bin"
Copy-Item "$ffmpegBin\*.dll" $staging
if (Test-Path "$ffmpegBin\ffmpeg.exe") { Copy-Item "$ffmpegBin\ffmpeg.exe" $staging }

foreach ($doc in @("README.md", "LICENSE", "scripts\QUICKSTART.txt")) {
    if (Test-Path $doc) { Copy-Item $doc $staging }
}

# --- Stage the bundled virtual display driver, if supplied -----------------------
Write-Host "[3/5] Looking for the virtual display driver..."
$vddDir = Join-Path $repo "installer\vendor\vdd"
$driverSetup = Get-ChildItem $vddDir -Filter "*setup*x64*.exe" -ErrorAction SilentlyContinue |
    Select-Object -First 1
$includeDriver = $false
if ($driverSetup) {
    $sig = Get-AuthenticodeSignature $driverSetup.FullName
    if ($sig.Status -ne "Valid") {
        Write-Warning "Driver setup signature is '$($sig.Status)', expected 'Valid'. Bundling anyway — verify the source."
    }
    $driverStage = Join-Path $staging "driver"
    New-Item -ItemType Directory -Path $driverStage | Out-Null
    Copy-Item $driverSetup.FullName (Join-Path $driverStage "vdd-setup-x64.exe")
    $includeDriver = $true
    Write-Host "      Bundling driver: $($driverSetup.Name)  [signature: $($sig.Status)]"
} else {
    Write-Warning "No driver found in installer\vendor\vdd\ (expected *setup*x64*.exe)."
    Write-Warning "Building an APP-ONLY installer. See installer\vendor\vdd\README.txt to bundle the driver."
}

# --- Compile the installer -------------------------------------------------------
Write-Host "[4/5] Compiling installer with Inno Setup..."
$issArgs = @(
    "/DStagingDir=$staging",
    "/DAppVersion=$version"
)
if ($includeDriver) { $issArgs += "/DIncludeDriver" }
$issArgs += (Join-Path $repo "installer\EternalMonitor.iss")

& $iscc @issArgs
if ($LASTEXITCODE -ne 0) { throw "Inno Setup compilation failed" }

# --- Report ----------------------------------------------------------------------
Write-Host "[5/5] Done."
$setupExe = Join-Path $outDir "EternalMonitor-Setup.exe"
if (Test-Path $setupExe) {
    $hash = (Get-FileHash $setupExe -Algorithm SHA256).Hash
    "{0:N2} MB" -f ((Get-Item $setupExe).Length / 1MB) | ForEach-Object { Write-Host "Size:   $_" }
    Write-Host "SHA256: $hash"
    Write-Host "Output: $setupExe"
    if (-not $includeDriver) { Write-Host "NOTE:   app-only build (no bundled driver)." }
} else {
    throw "Expected output not found: $setupExe"
}
