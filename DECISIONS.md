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

### `ffmpeg-next` with NVENC

Chosen because it gives a workable Rust integration for H.264 hardware encode on NVIDIA GPUs.

Current reality:

- The working path is `h264_nvenc`
- The repo currently depends on that path for successful streaming
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

That last change fixed large-frame corruption where `fragment_count` overflowed at `255`.

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

## Deferred decisions

These remain intentionally unresolved until the current WiFi UDP path is hardened:

- USB transport design
- Virtual display driver integration details
- Input relay protocol and host injection mechanism
- Multi-codec negotiation
