# EternalMonitor

Use your iPad as a real second monitor on Windows. Free, open-source, low-latency.

Windows sees a genuine extended display via a virtual display driver. Frames are GPU-encoded and streamed over WiFi or USB to the iPad, which decodes and renders them via Metal at up to 120Hz.

## How it works

1. **IddCx virtual display driver** makes Windows think an extra monitor is plugged in
2. **DXGI Desktop Duplication** captures the desktop as GPU textures
3. **NVENC** encodes frames on the GPU (H.264/H.265)
4. Frames are sent over **USB** (lowest latency) or **WiFi** (wireless convenience)
5. iPad decodes with **VideoToolbox** and renders with **Metal**

Target: sub-20ms glass-to-glass over USB, sub-35ms on WiFi.

## Repo layout

```
host/     Rust — Windows daemon (capture, encode, transport)
driver/   C — IddCx virtual display driver
ios/      Swift — iPad app (decode, render, input relay)
proto/    Shared protocol definitions (FlatBuffers)
```

## Building

Requires Rust stable with the MSVC toolchain on Windows.

The host encoder path also requires:
- FFmpeg 7.1 shared build on Windows
- LLVM/libclang available for `bindgen`

Example setup:

```powershell
$env:FFMPEG_DIR = "C:\path\to\ffmpeg-n7.1.x-win64-gpl-shared-7.1"
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cargo run -p eternal-host
```

```
cargo run -p eternal-host
```

Set `RUST_LOG=debug` for per-frame stats.

## Status

Phase 1 — DXGI capture loop is implemented. Encoding, transport, driver, and iPad app are not built yet. See [ROADMAP.md](ROADMAP.md) for the full plan.

## License

See [LICENSE](LICENSE).
