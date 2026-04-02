# EternalMonitor — Technical Decisions

## Language choices

### Rust (Windows host)
- Zero GC pauses — critical for consistent frame delivery
- `tokio` async runtime for transport without thread overhead
- `ffmpeg-next` crate gives full ffmpeg access with safe wrappers
- `windows` crate for DXGI / IddCx / WinUSB bindings
- **Rejected:** C++ (too much boilerplate, no package manager), C# (GC pauses)

### Swift (iPad)
- First-class VideoToolbox and Metal APIs
- `CADisplayLink` for precise 120Hz sync
- **Rejected:** React Native / Flutter (no access to VideoToolbox/Metal)

### C (driver)
- IddCx driver must be a Windows kernel-mode driver — C is the only practical choice
- Fork of `usbmmidd` as starting point

## Capture: DXGI Desktop Duplication
- Only API that gives dirty-rect metadata (skip encoding unchanged pixels)
- GPU surface — never copies to system RAM
- **Rejected:** GDI (CPU only, slow), BitBlt (CPU, no dirty rects), OBS hooks (licensing)

## Encoder: ffmpeg-next + NVENC
- NVENC provides hardware video encoding on supported NVIDIA GPUs with minimal CPU overhead
- H.264 Baseline chosen for universal iPad compatibility
- **Rejected:** custom NVENC bindings (too much work), software x264 (CPU cost)

## Transport: Custom UDP over VNC/RFB
- VNC/RFB (what wifiscreen uses) is fundamentally a remote desktop protocol —
  it wasn't designed for low-latency video streaming
- Custom UDP lets us drop frames rather than queue them (latency over reliability)
- **Rejected:** WebRTC (heavy, browser-centric), RTP (complex), plain TCP (HOL blocking)

## Protocol: FlatBuffers
- Zero-copy deserialization — no allocation in the hot path
- Strongly typed schema shared between Rust and Swift
- **Rejected:** protobuf (copy on parse), JSON (way too slow), raw bytes (no schema safety)

## Virtual display: IddCx
- Windows genuinely believes a second monitor is connected
- Apps render to it natively, display settings work, snapping works
- **Rejected:** virtual framebuffer mirror (wifiscreen approach — no real display)

## iPad transport: USB bulk vs WiFi UDP
- USB always preferred when cable detected (4–8ms vs 16–30ms)
- WiFi fallback uses same encoded stream, just different socket
- **Rejected:** Bluetooth (way too slow for video), AirPlay (closed protocol)
