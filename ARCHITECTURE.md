# EternalMonitor — Architecture

## Pipeline overview

```
[Windows desktop]
      │
      ▼
┌─────────────────────────────────────────────┐
│              host/  (Rust daemon)            │
│                                             │
│  DXGI Desktop Duplication                  │
│    └─ dirty-rect frame pull (GPU surface)  │
│         │                                  │
│  ffmpeg-next encoder (NVENC / H.264 / H.265) │
│    └─ GPU encode ~1–2ms per frame          │
│         │                                  │
│  tokio async transport                     │
│    ├─ UDP socket  (WiFi path)              │
│    └─ libusb bulk (USB path)              │
│                                             │
│  Connection broker                          │
│    ├─ mDNS discovery                       │
│    ├─ auth handshake                       │
│    └─ reconnect logic                      │
└─────────────────────────────────────────────┘
      │                        ▲
      │  encoded frames        │  HID input events
      ▼                        │
┌─────────────────────────────────────────────┐
│              ios/  (Swift app)               │
│                                             │
│  Receiver                                  │
│    ├─ Network Extension (WiFi)             │
│    └─ USB CDC / usbmuxd (USB)             │
│         │                                  │
│  VideoToolbox decoder                      │
│    └─ HW H.264/H.265 decode ~0.5ms        │
│         │                                  │
│  Metal renderer                            │
│    └─ CADisplayLink @ 120Hz               │
│         │                                  │
│  Input relay                               │
│    └─ touch/pencil → HID events → host    │
└─────────────────────────────────────────────┘
```

## Virtual display driver (driver/)

- Based on **IddCx** (Indirect Display Driver) Windows DDI
- Windows sees a real monitor — not a mirror
- Reports EDID matching iPad Pro resolution (2732×2048 or 1668×2388)
- Fork starting point: `usbmmidd` open-source IddCx driver
- Installed once; persists across reboots

## Capture (host/capture)

- **API:** DXGI Desktop Duplication (`IDXGIOutputDuplication`)
- Pulls frames as GPU textures — never touches CPU memory
- Dirty-rect metadata used to skip encoding unchanged regions
- Target: <1ms frame acquisition

## Encoder (host/encoder)

- **Crate:** `ffmpeg-next` with NVENC backend (NVIDIA GPU)
- Primary: H.264 Baseline (universal decode on iPad)
- Secondary: H.265 Main (higher quality mode, slightly higher decode cost)
- 1 frame of encode latency max — no B-frames, no lookahead
- Bitrate: adaptive, ~8–20 Mbps depending on content

## Transport (host/transport)

### WiFi (UDP)
- Raw UDP datagrams, MTU-aware fragmentation
- Lightweight reliability: selective NACK, no head-of-line blocking
- Estimated latency: 16–30ms glass-to-glass

### USB (bulk transfer)
- `libusb` on Windows host
- iPad side: custom USB CDC class or usbmuxd tunnel
- Estimated latency: 4–8ms glass-to-glass
- Automatically preferred when USB detected

## Protocol (proto/)

- **Format:** FlatBuffers (zero-copy, no parse overhead)
- Message types:
  - `FramePacket` — encoded video chunk + sequence number + timestamp
  - `InputEvent` — touch/pencil coords, pressure, HID keycode
  - `ControlMsg` — connect/disconnect/ping/display-config
  - `DisplayConfig` — resolution, refresh rate, HDR flag

## iPad app (ios/)

- **Language:** Swift
- **Decoder:** `VideoToolbox` VTDecompressionSession, async callback
- **Renderer:** Metal `MTKView` with `CADisplayLink` locked to 120Hz ProMotion
- **Buffer strategy:** 1-frame jitter buffer max; drop rather than queue
- **Input:** UITouch + Apple Pencil → serialize → send back on same socket

## Latency budget (USB target: <20ms)

| Stage | Budget |
|-------|--------|
| DXGI frame pull | ~0.5ms |
| NVENC H.264 encode | ~1.5ms |
| USB bulk transfer | ~4ms |
| VideoToolbox decode | ~0.5ms |
| Metal render + display | ~8ms (1 frame @ 120Hz) |
| **Total** | **~14.5ms** |
