# EternalMonitor

Use your iPad as a wireless second display for Windows, and control the PC from it.

[![CI](https://github.com/whoisaldo/EternalMonitor/actions/workflows/ci.yml/badge.svg)](https://github.com/whoisaldo/EternalMonitor/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/whoisaldo/EternalMonitor?labelColor=111&color=e8ff47)](https://github.com/whoisaldo/EternalMonitor/releases/latest)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?style=flat&labelColor=111)](https://eternalmonitor.dev)

A Rust host on the PC captures the desktop with DXGI, encodes on the GPU
(NVENC/AMF/QSV, H.264 or opt-in HEVC), and streams over UDP on the local
network. A native Swift app on the iPad decodes with VideoToolbox and renders
with Metal. Touch, Apple Pencil, and two-finger scroll travel back as mouse
input. MIT licensed.

Contributions welcome. Transport, encoders, rendering, docs, anything. Ping
`aldobenches285` on Discord to collaborate.

## What v0.2.0 does

- **Mirror or extend.** Mirror the primary display, capture a specific
  monitor, or stream a managed virtual extended display that exists only
  while the iPad is connected. The virtual display comes up at the iPad's
  native resolution and refresh rate.
- **Control the PC from the iPad.** Tap to click, drag to move the mouse,
  two fingers to scroll, hold for a right-click, Apple Pencil with pressure.
  Negotiated per session, with a view-only toggle in the app.
- **Protocol v2.** A real session (handshake with capability negotiation,
  busy rejection, liveness), host heartbeats, client keyframe requests,
  receiver reports, and NTP-style clock sync. The latency number in the HUD
  is measured, not guessed.
- **Reliability.** Adaptive bitrate under the host slider's ceiling, packet
  pacing on keyframe bursts, keyframe recovery after loss, automatic
  reconnect after signal loss, and supervisor-driven crash recovery on the
  host. An encoder crash restarts the pipeline in about a second without
  dropping the session.
- **Codecs.** H.264 everywhere. HEVC/H.265 is an experimental opt-in
  ("Prefer HEVC" in host Settings) with live mid-session switching.

Not in scope yet: audio, USB transport, a first-party display driver (the
extended display uses the bundled MIT-licensed
[Virtual-Display-Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)),
and an App Store listing. The iPad app installs via TestFlight or Xcode.

## Install (testers)

Grab **EternalMonitor-Setup.exe** from the
[latest release](https://github.com/whoisaldo/EternalMonitor/releases/latest),
run it, approve the one UAC prompt, and allow the firewall prompt for both
Private and Public networks. Step-by-step tester instructions live in
[scripts/QUICKSTART.txt](scripts/QUICKSTART.txt). The iPad app comes from
TestFlight (ask for an invite) or an Xcode build.

SmartScreen note: the installer is not code-signed yet. Click "More info",
then "Run anyway".

## Build from source

The workspace is two Rust crates (`host/`, plus `proto/` which builds the
pure `eternal-wire` protocol crate) and the Swift app in `ios/`.

### Windows host

Requirements: Rust stable (MSVC), an FFmpeg **7.1 shared** SDK, LLVM/libclang
for bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (folder containing bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

`scripts\build-installer.ps1` builds the full Setup.exe (needs Inno Setup and
the same `FFMPEG_DIR`). `scripts\package.ps1` builds the bare zip.

### macOS development loop (no Windows required)

The host builds and runs on macOS with a synthetic capture source. The
protocol, encoder, transport, and supervisor stack all run for real:

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit + golden-vector + synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

If Xcode's command-line tools are the selected developer directory, prefix
Xcode commands with `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

### iPad app

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

Open the generated project in Xcode to run on a physical iPad with your own
signing team.

### Full-system test on one Mac

```bash
./scripts/e2e_ios.sh                 # host (synthetic) → iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh   # same, over HEVC
```

The harness launches the headless host and the simulator app, auto-connects,
and asserts at least 120 decoded frames at the right resolution via the
app's machine-readable log milestones.

## How it's tested

- Golden wire vectors (`proto/testdata/`) parsed byte-for-byte by both the
  Rust and Swift codecs, plus fuzz "never crashes" tests on both sides.
- Pure-logic unit tests with injected clocks: session machine, ABR ladder,
  pacer, reassembly, input mapping, gesture state machine, supervisor.
- End-to-end tests that run the real pipeline. The Rust E2E covers
  handshake, lossy ABR step-down, encoder-crash recovery, input relay, and
  HEVC negotiation; the simulator harness above covers the full system.
- CI on every PR: Linux (wire crate), Windows (full host against pinned
  FFmpeg 7.1), macOS (full workspace including E2E), and the iOS simulator
  suite.

CI cannot verify real GPU encoders, the virtual display driver, or real
WiFi. [HARDWARE_VERIFICATION.md](HARDWARE_VERIFICATION.md) is the runbook
that covers those before each release.

## Repo layout

```text
host/       Rust host: capture, encode, transport, session, supervisor, egui GUI
proto/      eternal-wire crate: protocol v2 codecs, H.264/HEVC helpers, golden vectors
ios/        Swift iPad app (xcodegen project): receive, decode, render, input relay
installer/  Inno Setup script + bundled-driver staging for EternalMonitor-Setup.exe
scripts/    build-installer.ps1, package.ps1, e2e_ios.sh, QUICKSTART.txt
docs/       eternalmonitor.dev website (GitHub Pages)
```

## Environment variables (host)

| Variable | Effect |
| --- | --- |
| `ETERNAL_ENCODER` | Force an encoder (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1`/`0` overrides the HEVC preference (automation) |
| `ETERNAL_FPS` | Override target FPS |
| `ETERNAL_CAPTURE` | `synthetic` swaps DXGI for a generated test pattern |
| `ETERNAL_HEADLESS` | `1` runs without the GUI until SIGTERM/SIGINT |
| `ETERNAL_VDD_TIMEOUT_SECS` | Virtual-display attach timeout |
| `ETERNAL_ABR` | `0` disables adaptive bitrate |
| `ETERNAL_DROP` | Test-only: inject fractional datagram loss |
| `ETERNAL_AMF_DIAG` | `1` writes AMF bitstream diagnostics |
| `ETERNAL_LEGACY_PTS` | `1` restores the old frame-counter PTS (escape hatch) |

## Troubleshooting

- iPad can't connect: same WiFi (not a guest network), firewall allowed for
  Private and Public, and manual IP entry beats discovery on tricky
  networks. The host window shows the address and a QR code.
- Choppy video is almost always WiFi. Get near the router, prefer 5 GHz,
  wire the PC. The HUD's loss% and the host's bitrate readout tell the
  story.
- "H.264 (x264)" on the Stream tab means the hardware encoder failed to
  open and the host fell back to CPU encoding. Update GPU drivers and
  restart the stream.
- Version mismatch: protocol v2 is a clean break. A v0.1.x app or host
  shows a clear "update the other side" message instead of streaming.

## Reference docs

- [ARCHITECTURE.md](ARCHITECTURE.md) covers the pipeline, protocol v2, and design
- [DECISIONS.md](DECISIONS.md) explains why things are the way they are
- [RELEASE_NOTES.md](RELEASE_NOTES.md)
- [FRIENDS_TESTING.md](FRIENDS_TESTING.md) has organizer notes for beta testing
- [HARDWARE_VERIFICATION.md](HARDWARE_VERIFICATION.md) is the pre-release
  runbook for everything CI can't prove

## Credits

Built by Ali Younes ([@whoisaldo](https://github.com/whoisaldo)).

- Repository: [github.com/whoisaldo/EternalMonitor](https://github.com/whoisaldo/EternalMonitor)
- Questions & concerns: [aliyounes@eternalreverse.com](mailto:aliyounes@eternalreverse.com)

## License

Released under the MIT License. See [LICENSE](LICENSE). © 2026 Ali Younes.
