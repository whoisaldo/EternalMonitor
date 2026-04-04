## Unreleased

### What's new
- Multi-vendor GPU support: automatic detection of NVIDIA, AMD, and Intel GPUs via DXGI
- Encoder fallback chain: NVENC → AMF → QSV → libx264 (software)
- Per-encoder optimized low-latency settings
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
