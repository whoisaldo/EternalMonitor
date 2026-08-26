# EternalMonitor technical decisions

## Language choices

### Rust for the Windows host

- Good fit for a multi-stage capture/encode/transport pipeline
- Works well with `tokio`, `windows`, and `ffmpeg-next`
- Keeps the host codebase native and low-level without switching to C++

Rejected: C++ (more binding and build overhead for this repo shape), C#
(wrong fit for the native graphics and transport path).

### Swift for the iPad app

Direct access to VideoToolbox, Metal, Network, and UIKit/SwiftUI.

Rejected: React Native / Flutter. Wrong fit for this decode/render stack.

## Capture

### DXGI Desktop Duplication, CPU readback

The practical Windows desktop-capture API. The pipeline does one full-frame
copy (staging-texture readback into a recycled `Arc` buffer) and composites
the cursor on the CPU. Dirty-rect metadata is queried but not yet used for
partial encode. A zero-copy GPU path (capture texture straight into the
encoder) remains future work; measure first, because after the v0.2.0
hot-path work the readback is no longer the dominant cost at 1080p60.

## Encoder

### `ffmpeg-next` pinned to FFmpeg 7.1, multi-vendor hardware encode

The host detects the GPU vendor via DXGI adapter enumeration and walks a
vendor-preferred chain (NVENC / AMF / QSV, then libx264) with per-encoder
low-latency options. FFmpeg is pinned to 7.1 everywhere: the Windows DLLs,
CI's downloaded SDK, and Homebrew `ffmpeg@7`. The bytes tested are the bytes
shipped. Upgrading to 8.x is deliberately a separate change now that the E2E
harness exists to validate it.

AMF is the fragile path (startup keyframes without parameter sets, strict
VideoToolbox level requirements). Its guards were developed against real
hardware and get preserved verbatim through refactors.

### H.264 baseline default, HEVC opt-in

Baseline H.264 is the simplest compatibility target for VideoToolbox
startup. HEVC ships behind a host setting ("Prefer HEVC") until it has been
proven on each hardware encoder. Negotiation requires both the setting and
the client's advertised decode capability, and any HEVC open failure falls
back to H.264 silently.

### Encoder reconfiguration = session reopen

Hardware encoders ignore bitrate pokes on an open context, so every real
change (ABR rung, slider, codec switch) reopens the encoder session (50 to
200 ms, same stream epoch) and forces an IDR. The old per-frame
`apply_bitrate` call, which silently did nothing on NVENC and AMF, is gone.
The GUI value is now the ABR ceiling and is labeled accordingly.

## Transport & protocol

### Protocol v2: raw Annex B over custom UDP framing, one socket for media and control

v2 replaced the v1 format (FlatBuffers `FramePacket`, a 16-byte fragment
header, and a fire-and-forget `ETERNALHELLO`) as a clean break. Both sides
ship together, and each side recognizes the other's legacy traffic well
enough to say "update the other half". The break was the point: v1 had no
version field, no session identity, and no back channel, so compatible
evolution wasn't possible.

- FlatBuffers removed. The only dynamic field was the frame payload itself;
  width, height, and codec belong in the control plane (STREAM_CONFIG). So
  media is a fixed 32-byte header plus raw Annex B. One less serialization
  layer on the per-frame hot path, one less parser to fuzz.
- No app-layer checksum. UDP's checksum plus magic/version/session checks
  and strict length validation catch stray and truncated datagrams. A
  corrupted-but-valid datagram costs at most an artifact until the next
  keyframe, which the recovery path requests anyway. A CRC would tax every
  packet to protect against the rarest failure with the mildest
  consequence.
- FEC deferred. Consumer-WiFi loss is bursty, which single-XOR parity
  handles poorly. Pacing, keyframe recovery, and ABR carry v0.2.0; packet
  types and flag bits stay reserved for FEC if measurement ever justifies
  it.
- Client liveness is its receiver reports (500 ms cadence) rather than a
  dedicated heartbeat message. The reports must flow anyway to drive ABR.

Rejected: TCP (head-of-line blocking), WebRTC (too heavy), RTP/RTCP (we
would use a fraction of it and still need custom extensions for input and
config).

### Adaptive bitrate on the host, signals from the client

The host owns the ladder because it owns the encoder; the client just
reports what it sees. Loss or keyframe-request pressure steps down,
sustained clean reports step up, and the GUI slider caps the ladder.

## Discovery

### mDNS/DNS-SD, best effort, manual IP as the guaranteed path

The host advertises `_eternaldisplay._udp` (TXT: `version`, `proto=2`,
`platform`), re-upserting every 60 s. The v0.1.x refresh unregistered
first, which created a periodic discovery hole. Exit sends goodbye packets.
Multicast-filtering networks still exist, so manual IP and the QR code stay
first-class.

## Virtual display

### Bundled third-party Indirect Display Driver, managed on demand

The extended display drives the bundled
[VirtualDrivers/Virtual-Display-Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver).
Its MIT license is verified, and its license text ships in the installer
next to the driver. The host doesn't run elevated, so the installer
registers two SYSTEM scheduled tasks (enable/disable) that the host
triggers via `schtasks`, avoiding a per-toggle UAC prompt.

- Enabled only while an iPad is connected. Disabled on exit, target change,
  startup, panic, and on client loss (the v2 liveness signal finally made
  idle teardown possible).
- Before enabling, the host writes `vdd_settings.xml` so the virtual
  display offers the iPad's native landscape resolution and refresh. There
  is an opt-out in Settings.
- A first-party signed display driver remains a long-term goal.

## Input relay

### Normalized coordinates on the wire, a pure gesture machine on the client, `SendInput` on the host

Touches are normalized over the displayed video (letterbox-corrected), which
makes the wire format resolution-independent. Tap-vs-drag-vs-scroll
disambiguation is a pure state machine: tap clicks on release, a drag
commits after slop, and the Pencil presses immediately because ink can't
wait out a disambiguation window. Edges are sent twice with one event id and
deduped host-side, which buys loss tolerance without retransmit machinery.
Injection maps through the captured output's desktop rectangle, so
multi-monitor and virtual-display layouts land clicks on the right screen.

## Versioning & release

- One version, single-sourced from `host/Cargo.toml` (`env!` into the
  banner and the mDNS TXT), matched by the iOS `MARKETING_VERSION`.
- CI builds releases from a `v*` tag: pinned FFmpeg, pinned driver version,
  hard-fail Authenticode verification on the bundled driver, and the
  installer's SHA-256 published in the release body, where the website
  reads it.

## Deferred

- Audio (a WASAPI → Opus → AVAudioEngine sketch exists; wire types are
  reserved)
- USB transport
- First-party signed display driver
- FEC (types and flags reserved), zero-copy GPU capture, dirty-rect encode
