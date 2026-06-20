# EternalMonitor

## ⚡ Contribute
** OPEN TO CONTRIBUTIONS.** 

Whether you want to optimize the network transport layer, refine the Metal rendering pipeline, or improve Windows capture efficiency, your PRs are highly welcome. 

Any help would be appreciated! :blush:

> 💬 **Help build this together. Ping me directly on Discord to collaborate:** **`aldobenches285`**

[![Version](https://img.shields.io/badge/version-v0.1.2--mirror-e8ff47?style=flat&labelColor=111)](https://github.com/whoisaldo/EternalMonitor/releases/tag/v0.1.2-mirror)
[![Download](https://img.shields.io/badge/download-installer-blue?style=flat&labelColor=111)](https://github.com/whoisaldo/EternalMonitor/releases/download/v0.1.2-mirror/EternalMonitor-Setup.exe)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?style=flat&labelColor=111)](https://eternalmonitor.dev)

**Website:** [eternalmonitor.dev](https://eternalmonitor.dev)

Use your iPad as a low-latency Windows display receiver over local-network UDP.

This repo currently contains a working Windows host capture/encode/transport path and a working iPad receive/decode/render path. The known-good stream state is commit `bc44770` on branch `feature/rust-workspace-bootstrap`.

## Current status

Implemented now:

- Windows host captures a **selectable display output** (primary by default) with DXGI Desktop Duplication
- Capture-display picker in the Settings tab — stream any output, including a **virtual extended display** created by a signed Indirect Display Driver, so the iPad can be a true second screen instead of only a mirror. The virtual display is brought up **on demand, only while an iPad is connected**, and removed on exit — no phantom monitor when idle
- Host converts BGRA to YUV420P and can select hardware H.264 encoders per GPU vendor
- Host advertises an mDNS/DNS-SD service and streams frames over UDP on port `9876`
- iPad app receives fragmented UDP datagrams, reassembles `FramePacket` payloads, decodes with VideoToolbox, and renders with Metal
- Direct connect by IP works end to end with the current transport format
- One-step Windows installer (`EternalMonitor-Setup.exe`) that bundles the host, FFmpeg runtime, and the virtual display driver for non-technical testers

Not done yet:

- First-party signed display driver (today the host drives a third-party signed virtual display driver)
- USB transport
- Input relay back to Windows
- Reliability layer beyond basic UDP fragmentation/reassembly
- Production-grade zero-config discovery

Known caveat:

- The iPad "Scan network" path may fail to find the host even when direct IP connect works. Treat discovery as incomplete and use manual IP entry (or the QR code) when needed.
- NVIDIA (NVENC), AMD (AMF), and Intel (QSV) encode paths are implemented and hardened for the iPad VideoToolbox decoder; AMD is the current focus of beta testing. If a hardware encoder can't open, the host falls back to CPU (libx264) and shows a warning banner on the Stream tab.

## How it works

1. Windows host captures the desktop with DXGI Desktop Duplication
2. Frames are read back, converted, and encoded with hardware H.264 (auto-detected per GPU vendor, with NVIDIA currently the verified path)
3. Encoded `FramePacket` payloads are fragmented into UDP datagrams
4. The iPad app reassembles and parses those payloads
5. VideoToolbox decodes frames and Metal renders them

Current target is practical local-network streaming, not a finished second-monitor product yet.

## Repo layout

```text
host/       Rust Windows host: capture, encode, UDP transport, mDNS advertisement, egui GUI
ios/        Swift iPad app: connect UI, UDP receive, reassembly, decode, render
proto/      Shared protocol serialization code and schemas
installer/  Inno Setup script + bundled-driver staging for EternalMonitor-Setup.exe
scripts/    package.ps1 (zip), build-installer.ps1 (Setup.exe), QUICKSTART.txt
docs/       eternalmonitor.dev website (GitHub Pages)
```

## Build and run

Requires Windows with Rust stable MSVC and FFmpeg 7.1 for the host (GPU encoding auto-detected). The iOS app must be built on macOS with Xcode for a physical device.

### Windows host

```powershell
cargo build -p eternal-host
& "C:\Users\aliyo\OneDrive\Desktop\EternalMonitor\target\debug\eternal-host.exe"
```

If you need to change the listen port:

```powershell
& "C:\Users\aliyo\OneDrive\Desktop\EternalMonitor\target\debug\eternal-host.exe" 9876
```

### iOS app

The iOS project is generated with XcodeGen:

```bash
cd ios
xcodegen generate
```

Then open the generated Xcode project on macOS, build the `EternalMonitor` target, and run it on a physical iPad.

## Installer (for testers)

Non-technical testers don't need Rust or the zip. Build a single `EternalMonitor-Setup.exe`:

```powershell
.\scripts\build-installer.ps1
```

It compiles the release host, bundles the FFmpeg runtime, and — if a signed virtual
display driver is staged in `installer/vendor/vdd/` (see that folder's `README.txt`) —
bundles it too. The tester double-clicks the installer, approves one Windows (UAC)
prompt, and gets the app plus the virtual display installed in one run. The driver
install always requires that single elevation prompt; a fully seamless first-party
driver is a future (v0.2.0) goal.

Tester-facing instructions live in [scripts/QUICKSTART.txt](scripts/QUICKSTART.txt).

## Troubleshooting

- If the iPad says no complete frame was reassembled, make sure both the Windows host and the iPad app were rebuilt from the same revision. The UDP fragment header changed in the working transport fix.
- If scan finds nothing, try direct IP connect first. Discovery failure does not necessarily mean streaming is broken.
- If you're testing on AMD, use the latest `v0.1.2-mirror` build. The AMF path prepends fresh SPS/PPS on every random-access frame (including forced non-IDR intra frames), recovers the startup keyframe if parameter sets aren't ready, and writes a first-120-packet capture to `%APPDATA%\EternalMonitor\diagnostics\` for offline inspection.
- If the host binary fails to rebuild on Windows with access denied for `eternal-host.exe`, close the running GUI process first.

## Reference docs

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [DECISIONS.md](DECISIONS.md)
- [RELEASE_NOTES.md](RELEASE_NOTES.md)
- [FRIENDS_TESTING.md](FRIENDS_TESTING.md) — organizer notes for beta testing

## Credits

Built by Ali Younes ([@whoisaldo](https://github.com/whoisaldo)).

- Repository: [github.com/whoisaldo/EternalMonitor](https://github.com/whoisaldo/EternalMonitor)
- Questions & concerns: [aliyounes@eternalreverse.com](mailto:aliyounes@eternalreverse.com)

## License

Released under the MIT License — see [LICENSE](LICENSE). © 2026 Ali Younes.
