## EternalMonitor v0.2.0

A ground-up revamp of the streaming core. Clean break: the v0.2.0 host and
iPad app only work with each other. Each side shows a clear "update the
other half" message if it meets a v0.1.x peer.

### Control the PC from the iPad
- Tap to click, drag to move the mouse, two-finger scroll, half-second hold
  for a right-click, Apple Pencil with pressure. On by default ("Control PC
  with touch" in the iPad Settings) and negotiated per session; the host
  never injects for a session that didn't ask.
- While control is on, a three-finger tap toggles the stats HUD.

### Protocol v2
- A real session: handshake with capability negotiation, busy rejection for a
  second device, instant reconnect takeover, liveness tracking, and clean
  goodbyes (including when the app is backgrounded).
- Host heartbeats, client receiver reports, keyframe requests, and NTP-style
  clock sync. The HUD's latency number is now a real end-to-end measurement.
- Media is raw Annex B in a fixed 32-byte header; FlatBuffers is gone.

### Reliability
- Adaptive bitrate: the host slider is now the ceiling, and the stream steps
  down under loss and back up when the network recovers.
- Keyframe recovery after loss (client-requested, host rate-limited), packet
  pacing on keyframe bursts, and automatic reconnect with backoff after
  "SIGNAL LOST".
- Host supervisor v2: an encoder crash auto-restarts the pipeline in about a
  second and the iPad resumes on the same session, with no reconnect and no
  re-handshake. Wedge watchdogs catch silent stalls.

### Video
- HEVC/H.265 as an experimental opt-in ("Prefer HEVC" on the host):
  negotiated per client, live mid-session codec switching, automatic H.264
  fallback.
- Real capture-time PTS (rate control finally sees true frame cadence), NV12
  decode output with proper BT.601/709 handling, aspect-fit rendering, and
  draw-on-demand (no more free-running 120 Hz redraw).
- The extended (virtual) display now offers the iPad's native resolution
  and refresh rate, and tears down when the client disconnects.

### Quality of life
- Settings apply from the first frame (including headless runs), atomic
  settings writes, mDNS advertisement without the periodic re-registration
  gap plus goodbye packets on exit, live host info (name, resolution, codec,
  bitrate) in the iPad Settings, "Keep screen awake" and "Resume after
  switching apps" toggles, VoiceOver labels, and the Syne display font
  actually rendering.
- The whole stack is now covered by tests: golden wire vectors parsed
  byte-for-byte by both languages, pure-logic suites with injected clocks,
  and end-to-end tests (including a full host→simulator stream in both
  codecs) running in CI on Linux, Windows, and macOS.

---

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
