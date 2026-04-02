# EternalMonitor — Build Roadmap

## Phase 1 — Capture pipeline (host/)
- [ ] Scaffold Rust workspace (`host/`, `proto/` crates)
- [ ] DXGI Desktop Duplication loop — pull frames, log frame times
- [ ] Dirty-rect extraction and region skipping
- [ ] Basic frame stats CLI output (fps, latency, dropped frames)

**Exit criteria:** Stable 60fps DXGI capture loop with <1ms pull time logged

## Phase 2 — Encode pipeline (host/)
- [ ] `ffmpeg-next` integration with NVENC backend
- [ ] H.264 Baseline encode of DXGI frames
- [ ] Encode latency benchmarking (<2ms target)
- [ ] H.265 mode flag

**Exit criteria:** Full frames encoding at 60fps with GPU, <2ms per frame

## Phase 3 — Transport (host/ + proto/)
- [ ] FlatBuffers schema (`FramePacket`, `ControlMsg`, `DisplayConfig`)
- [ ] UDP sender with MTU fragmentation
- [ ] Selective NACK reliability layer
- [ ] USB bulk sender via libusb
- [ ] Auto-detect USB vs WiFi and switch

**Exit criteria:** Encoded frames arriving at a test receiver with <5ms transport overhead on USB

## Phase 4 — Virtual display driver (driver/)
- [ ] Fork and build `usbmmidd` IddCx base
- [ ] Custom EDID for iPad Pro resolution (2732×2048)
- [ ] Driver signing / test-signing mode for dev
- [ ] Integration with host daemon (driver reports new display → daemon starts capture)

**Exit criteria:** Windows shows a second monitor at iPad resolution, apps can target it

## Phase 5 — iPad app MVP (ios/)
- [ ] Swift package scaffold
- [ ] UDP receiver + FlatBuffers decode
- [ ] VideoToolbox VTDecompressionSession for H.264
- [ ] Metal MTKView renderer, CADisplayLink 120Hz
- [ ] Frames appearing on iPad screen

**Exit criteria:** Live video from Windows desktop rendering on iPad at 60fps

## Phase 6 — USB path (ios/)
- [ ] USB CDC / usbmuxd tunnel on iPad side
- [ ] Switch transport layer based on connection type
- [ ] Latency measurement end-to-end

**Exit criteria:** Sub-20ms glass-to-glass over USB

## Phase 7 — Input relay (ios/ + host/)
- [ ] Capture UITouch + Apple Pencil events on iPad
- [ ] Serialize as `InputEvent` FlatBuffer, send back over same connection
- [ ] Deserialize on host, inject as HID events via Windows `SendInput`

**Exit criteria:** Touch on iPad moves cursor on Windows

## Phase 8 — Polish + release
- [ ] Settings UI (resolution, quality, transport preference)
- [ ] Auto-reconnect on cable plug/unplug
- [ ] mDNS discovery (zero-config pairing on WiFi)
- [ ] GitHub Actions CI (Rust tests + Swift build)
- [ ] README, install guide, driver signing instructions
- [ ] First public release

## Dev environment
- **Windows:** Rust stable, MSVC toolchain, Windows DDK (for driver)
- **iPad:** Xcode, Swift 5.9+, physical device required (no Simulator for Metal/VideoToolbox)
- **WSL:** Rust toolchain for cross-platform logic / proto codegen only
  (DXGI and IddCx require native Windows — do not develop capture/driver in WSL)
