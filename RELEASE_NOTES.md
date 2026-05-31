## EternalMonitor v0.1.2-mirror

Reliability release focused on the AMD encode path and a seamless, on-demand extended display.

### Extended display & virtual-display lifecycle
- **On-demand only:** the bundled virtual display driver now turns on **only once an iPad
  actually connects** and is torn down on exit — so an idle PC never shows a phantom second
  monitor even when "Extended display" is the saved capture target.
- **Crash-safe:** the virtual display is disabled unconditionally at startup and via a panic
  hook, so a crash or force-kill can't strand a phantom monitor.
- **Robust device control:** the installer's enable/disable scheduled tasks now resolve the
  virtual-display device at trigger time (name-agnostic) instead of baking a guessed device id,
  and the installer verifies the tasks registered.
- **Correct output:** on a PC that already has a second monitor, selecting the extended display
  no longer grabs the wrong screen; and the driver is disabled if it fails to attach.
- **Idle keepalive:** a blank/static extended display still delivers a first keyframe, so the
  iPad connects instead of timing out on black.

### AMD / encoding
- AMF now prepends SPS/PPS on forced non-IDR intra frames (not just IDRs), and retries the
  startup keyframe if parameter sets aren't available yet — fewer black-screen/desync cases.
- Configurable virtual-display attach timeout (`ETERNAL_VDD_TIMEOUT_SECS`).

### Diagnostics & networking
- Session log and the AMF packet capture now write to `%APPDATA%\EternalMonitor\{logs,
  diagnostics}` so they work under Program Files (they previously failed silently there).
- Per-frame log spam removed from the hot path.
- Fragment header now carries a per-run stream epoch for instant restart resync; the iPad's
  connect timeout extends once data starts flowing on a slow/jittery network.
- The displayed address / QR fall back to local-adapter enumeration when there's no default
  route, and the GUI shows a banner if the extended display falls back to mirroring.

### Carried forward from the in-development line
- Selectable capture display picker (Settings tab), active-capture-source readouts, the one-step
  Windows installer (`EternalMonitor-Setup.exe`), and GUI/iPad credits.

---

## EternalMonitor v0.1.1-mirror

### What's new
- Multi-vendor GPU support: automatic detection of NVIDIA, AMD, and Intel GPUs via DXGI
- Encoder fallback chain: NVENC → AMF → QSV → libx264 (software)
- Per-encoder optimized low-latency settings
- AMD H.264 hotfix: AMF output is now normalized for VideoToolbox decode, with AMF-only flag guards and stronger bitstream diagnostics
- Startup banner showing detected GPU, encoder, and listen address
- Suppressed mDNS multicast log spam from Tailscale interfaces

---

## EternalMonitor v0.1.0-mirror

First public release. Use your iPad as a wireless second monitor for Windows over local WiFi.

### What works
- Desktop mirroring at up to 60fps
- H.264 hardware encoding via NVENC (NVIDIA GPU required)
- WiFi transport over UDP
- Native Metal rendering on iPad at 120Hz ProMotion
- Auto-discovery via mDNS (or manual IP entry)

### Known limitations
- Mirror only — extended display requires the virtual driver (coming in v0.2.0)
- WiFi only — USB transport coming in v0.2.0
- No touch/input relay yet — coming in v0.3.0
- NVIDIA GPU required for this release — AMD support planned

### Requirements
- Windows 10 or 11 (64-bit)
- NVIDIA RTX GPU
- iPad running iPadOS 16 or later
- Both devices on the same WiFi network

### Installation
Download `EternalMonitor-v0.1.0-mirror-windows.zip`, extract, run `EternalMonitor-host.exe`, open the iPad app, enter the IP shown, tap Connect.
