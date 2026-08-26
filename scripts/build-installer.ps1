# EternalMonitor installer build script
# Produces a single EternalMonitor-Setup.exe that installs the host app, the FFmpeg
# runtime, and (if the driver is present) the bundled virtual display driver.
#
# Usage:  .\scripts\build-installer.ps1
#
# To bundle the virtual display driver, drop the signed setup into
#   installer\vendor\vdd\   (see that folder's README.txt for the exact file).
# Without it, the build still succeeds and produces an app-only installer.

param(
    # Fail the build on a driver whose Authenticode signature isn't Valid.
    # Today's upstream setup wrapper is unsigned (CI pins its SHA-256 in
    # release.yml instead), so leave this off unless you are bundling a
    # signed driver build and want the signature enforced.
    [switch]$StrictSignature
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$version = [regex]::Match((Get-Content -Raw "host\Cargo.toml"), 'version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $version) { throw "Could not read the package version from host\Cargo.toml" }
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
cargo build --release --locked -p eternal-host
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# --- Stage the payload -----------------------------------------------------------
Write-Host "[2/5] Staging payload -> $staging"
if (Test-Path $staging) { Remove-Item $staging -Recurse -Force }
New-Item -ItemType Directory -Path $staging | Out-Null
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

Copy-Item "target\release\eternal-host.exe" (Join-Path $staging "EternalMonitor-host.exe")

# FFmpeg runtime DLLs + utility. FFMPEG_DIR (the same env var the build uses)
# wins; .cargo\config.toml is the fallback for developer machines that pin it there.
$ffmpegDir = $env:FFMPEG_DIR
if (-not $ffmpegDir -and (Test-Path ".cargo\config.toml")) {
    $cargoConfig = Get-Content -Raw ".cargo\config.toml"
    $m = [regex]::Match($cargoConfig, 'FFMPEG_DIR\s*=\s*\{\s*value\s*=\s*"([^"]+)"')
    if (-not $m.Success) { $m = [regex]::Match($cargoConfig, 'FFMPEG_DIR\s*=\s*"([^"]+)"') }
    if ($m.Success) { $ffmpegDir = $m.Groups[1].Value }
}
if (-not $ffmpegDir) { throw "FFMPEG_DIR is not set. Point it at your FFmpeg 7.1 shared SDK (the folder containing bin\avcodec-*.dll)." }
$ffmpegBin = Join-Path $ffmpegDir "bin"
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
        if ($StrictSignature) {
            throw "Driver setup signature is '$($sig.Status)', expected 'Valid'. Refusing to bundle an unverified driver in a release build."
        }
        Write-Warning "Driver setup signature is '$($sig.Status)', expected 'Valid'. Bundling anyway — verify the source."
    }
    $driverStage = Join-Path $staging "driver"
    New-Item -ItemType Directory -Path $driverStage | Out-Null
    Copy-Item $driverSetup.FullName (Join-Path $driverStage "vdd-setup-x64.exe")
    # Redistribute the driver's license alongside it (it's a separate MIT
    # project we bundle, not code we link).
    Get-ChildItem $vddDir -Filter "LICENSE*" -ErrorAction SilentlyContinue |
        ForEach-Object { Copy-Item $_.FullName $driverStage }
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
