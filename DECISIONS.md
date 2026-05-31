# EternalMonitor — Technical Decisions

## Language choices

### Rust for the Windows host

- Good fit for a multi-stage capture/encode/transport pipeline
- Works well with `tokio`, `windows`, and `ffmpeg-next`
- Keeps the host codebase native and low-level without switching to C++

Rejected:

- C++: more binding and build overhead for this repo shape
- C#: less suitable for the current native graphics and transport path

### Swift for the iPad app

- Direct access to VideoToolbox, Metal, Network, and UIKit/SwiftUI APIs
- Best fit for hardware decode and native iPad rendering

Rejected:

- React Native / Flutter: wrong fit for this decode/render stack

## Capture

### DXGI Desktop Duplication

Chosen because it is the practical Windows desktop capture API for this project.

Current reality:

- It is working
- The current implementation copies into a CPU-readable staging texture
- Dirty rect metadata is queried, but the pipeline does not yet exploit it for partial encode

Earlier docs overstated this as a never-touch-CPU path. That is not true in the current build.

## Encoder

### `ffmpeg-next` with multi-vendor hardware encode

The host detects the GPU vendor via DXGI adapter enumeration and selects the best
available hardware encoder using a vendor-preferred fallback chain:

1. Vendor-preferred encoder (NVENC for NVIDIA, AMF for AMD, QSV for Intel)
2. Other hardware encoders in order: NVENC → AMF → QSV
3. Software fallback: libx264

Each encoder has tuned low-latency options (`gpu.rs` resolves the encoder,
`encoder/mod.rs` applies per-encoder settings). The GPU with the most dedicated
VRAM is selected automatically; software adapters are excluded.

Current reality:

- NVIDIA (h264_nvenc), AMD (h264_amf), Intel (h264_qsv), and software (libx264) paths are implemented
- H.265 is not implemented in the active iPad path yet

### H.264 Baseline

Chosen for the current iPad decode path because it is the simplest compatibility target for VideoToolbox bootstrap and stream startup.

## Transport

### Custom UDP framing

Chosen because the project cares more about low latency than guaranteed in-order delivery.

Current reality:

- The repo currently implements custom UDP fragmentation and reassembly
- There is no selective NACK layer yet
- There is no USB transport yet
- The active wire format uses a `16` byte fragment header with `u16` fragment index/count fields
- The header's final 4 bytes carry a per-pipeline-run `stream_epoch` so the receiver detects a
  stream restart immediately; the bytes were previously reserved/zero, so older receivers that
  ignore them remain wire-compatible

That `u16` change fixed large-frame corruption where `fragment_count` overflowed at `255`.

Rejected for now:

- TCP: head-of-line blocking is the wrong tradeoff here
- WebRTC: too heavy for the current stage
- RTP: more complexity than the current implementation needs

## Protocol

### FlatBuffers

Chosen because the host and iPad both need a compact binary packet format with clear field structure.

Current reality:

- `FramePacket` is in active use
- Swift currently uses a manual parser matched to the Rust serializer
- The broader protocol families described in older planning docs are not all implemented yet

## Discovery

### mDNS/DNS-SD

Chosen for zero-config host discovery on local networks.

Current reality:

- The host advertises a Bonjour service
- The iPad scans for that service
- Direct IP connect is more reliable than discovery at the moment

So discovery is still an incomplete feature, not something to depend on.

## Virtual display

### Bundled third-party Indirect Display Driver, managed on demand

The extended-display feature drives the bundled VirtualDrivers/Virtual-Display-Driver. The host
does not run elevated, so the installer registers two SYSTEM scheduled tasks (enable/disable) that
the host triggers via `schtasks /Run` — no per-toggle UAC prompt.

Current reality:

- The device is left disabled by default and is enabled **only once an iPad connects**, then
  disabled on exit / target change / startup and via a panic hook — so it never lingers as a
  phantom monitor.
- The tasks resolve the device at trigger time (name-agnostic) rather than baking a fixed
  instance id, so they survive driver-version and PnP-enumeration differences.

A first-party signed display driver (removing the third-party dependency) is still a v0.2.0 goal.

## Deferred decisions

These remain intentionally unresolved until the current WiFi UDP path is hardened:

- USB transport design
- First-party signed display driver (today the host manages a bundled third-party VDD)
- Input relay protocol and host injection mechanism
- Idle-disconnect teardown of the virtual display (needs a bidirectional iPad heartbeat)
