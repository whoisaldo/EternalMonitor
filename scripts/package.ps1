# EternalMonitor release packaging script (zip, no installer)
# Usage: .\scripts\package.ps1
#
# FFmpeg runtime location: set the FFMPEG_DIR environment variable (the same
# one the build itself uses). If unset, falls back to .cargo\config.toml for
# developer machines that pin it there.

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$version = [regex]::Match((Get-Content -Raw "host\Cargo.toml"), 'version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $version) { throw "Could not read the package version from host\Cargo.toml" }
$distDir = "dist"
$zipName = "EternalMonitor-v$version-windows.zip"

function Resolve-FfmpegDir {
    if ($env:FFMPEG_DIR) { return $env:FFMPEG_DIR }
    if (Test-Path ".cargo\config.toml") {
        $cargoConfig = Get-Content -Raw ".cargo\config.toml"
        $m = [regex]::Match($cargoConfig, 'FFMPEG_DIR\s*=\s*\{\s*value\s*=\s*"([^"]+)"')
        if (-not $m.Success) { $m = [regex]::Match($cargoConfig, 'FFMPEG_DIR\s*=\s*"([^"]+)"') }
        if ($m.Success) { return $m.Groups[1].Value }
    }
    throw "FFMPEG_DIR is not set. Point it at your FFmpeg 7.1 shared SDK (the folder containing bin\avcodec-*.dll)."
}

Write-Host "[1/5] Building release binary..."
cargo build --release --locked -p eternal-host
if ($LASTEXITCODE -ne 0) { exit 1 }

Write-Host "[2/5] Preparing dist/..."
if (Test-Path $distDir) { Remove-Item $distDir -Recurse -Force }
New-Item -ItemType Directory -Path $distDir | Out-Null

Write-Host "[3/5] Copying files..."
Copy-Item "target\release\eternal-host.exe" "$distDir\EternalMonitor-host.exe"
$ffmpegBin = Join-Path (Resolve-FfmpegDir) "bin"
Copy-Item "$ffmpegBin\*.dll" "$distDir\"
Copy-Item "$ffmpegBin\ffmpeg.exe" "$distDir\"
Copy-Item "README.md" "$distDir\"
Copy-Item "LICENSE" "$distDir\"
Copy-Item "scripts\QUICKSTART.txt" "$distDir\"

Write-Host "[4/5] Zipping..."
Compress-Archive -Path "$distDir\*" -DestinationPath $zipName -Force

Write-Host "[5/5] SHA256..."
$hash = (Get-FileHash $zipName -Algorithm SHA256).Hash
Write-Host "SHA256: $hash"
Write-Host "Done: $zipName"
