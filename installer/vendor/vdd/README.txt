Virtual Display Driver — bundled installer payload
==================================================

The single EternalMonitor-Setup.exe bundles a third-party signed virtual display
driver so non-technical testers never have to visit GitHub or install a driver by
hand. You only need to add the driver file here ONCE; after that, every installer
build picks it up automatically.

WHAT TO DOWNLOAD
----------------
1. Open:
   https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/25.5.2
2. Download this asset (the signed x64 setup, ~5 MB):
   Virtual.Display.Driver-v25.05.03-setup-x64.exe
3. Save it into THIS folder:
   installer\vendor\vdd\

That's it. The build script (scripts\build-installer.ps1) auto-detects any file
matching  *setup*x64*.exe  in this folder, stages it as  driver\vdd-setup-x64.exe
inside the installer, and the installer runs it (elevated, no second UAC) during
setup. If no such file is present, the build still succeeds but produces an
APP-ONLY installer (no bundled driver) and prints a clear warning.

LICENSE / ATTRIBUTION
---------------------
The Virtual Display Driver is a separate third-party program by the VirtualDrivers
project (https://github.com/VirtualDrivers/Virtual-Display-Driver). It is merely
bundled and launched by our installer — not linked into EternalMonitor. Keep its
LICENSE alongside the binary when redistributing. Verify the project's current
license on its repository before public distribution.

VERIFY BEFORE BUNDLING
----------------------
Upstream ships this setup wrapper UNSIGNED (Get-AuthenticodeSignature
reports NotSigned) — the signed part is the driver package inside it, which
Windows itself validates at install time. Verify the download by hash
instead; the vetted 25.5.2 asset is:
   SHA-256: ca10b85babecfb636c85b3f04d2306968d4f940dd3dd35767f866207bfba846e
   (Get-FileHash .\Virtual.Display.Driver-v25.05.03-setup-x64.exe)
The release workflow pins this same hash and fails the build on mismatch.

NOTE: files in this folder (other than this README) are git-ignored so the binary
is not committed to the repository.
