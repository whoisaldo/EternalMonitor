# EternalMonitor release packaging script
# Usage: .\scripts\package.ps1

$version = "v0.1.0-mirror"
$distDir = "dist"
$zipName = "EternalMonitor-$version-windows.zip"

Write-Host "[1/5] Building release binary..."
cargo build --release -p eternal-host
if ($LASTEXITCODE -ne 0) { exit 1 }

Write-Host "[2/5] Preparing dist/..."
if (Test-Path $distDir) { Remove-Item $distDir -Recurse -Force }
New-Item -ItemType Directory -Path $distDir | Out-Null

Write-Host "[3/5] Copying files..."
Copy-Item "target\release\eternal-host.exe" "$distDir\EternalMonitor-host.exe"
$ffmpegBin = (Select-String -Path ".cargo\config.toml" -Pattern 'FFMPEG_DIR\s*=\s*"(.+)"').Matches[0].Groups[1].Value + "\bin"
Copy-Item "$ffmpegBin\*.dll" "$distDir\"
Copy-Item "README.md" "$distDir\"
Copy-Item "LICENSE" "$distDir\"

Write-Host "[4/5] Zipping..."
Compress-Archive -Path "$distDir\*" -DestinationPath $zipName -Force

Write-Host "[5/5] SHA256..."
$hash = (Get-FileHash $zipName -Algorithm SHA256).Hash
Write-Host "SHA256: $hash"
Write-Host "Done: $zipName"
